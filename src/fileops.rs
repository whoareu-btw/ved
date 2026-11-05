//! What the project panel does to files: make a file or a folder, delete one.
//!
//! These are the only places where `ved` changes the file system on its own account, so they are
//! strict. A name is checked before anything is touched (no absolute paths, no `..`, no control
//! characters, nothing inside `.git`), the folder it goes in must really be inside the project (after
//! resolving links), a link is never followed, an existing file is never overwritten (the file is
//! created with `O_EXCL`), and a deletion only goes through if the thing is still what the person was
//! shown.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The longest a single name may be (what file systems allow) and the longest thing typed.
const MAX_PART_BYTES: usize = 255;
const MAX_TYPED_BYTES: usize = 4000;

/// A name as typed, taken apart: `src/deep/file.rs` is `["src", "deep", "file.rs"]`; a trailing `/`
/// says the last part is a folder.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct NewName {
    pub parts: Vec<String>,
    pub is_dir: bool,
}

/// What an I/O error says, without the "(os error 13)" that `Display` appends.
pub fn io_msg(e: &io::Error) -> String {
    let s = e.to_string();
    match s.rfind(" (os error ") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}

/// Is this name git's folder? On a file system that ignores case (the macOS default) `.GIT` is the same
/// folder, so case is ignored.
fn is_git(name: &str) -> bool {
    name.eq_ignore_ascii_case(".git")
}

/// Check and split a typed name. Only what could never do harm gets through.
pub fn parse_name(input: &str) -> Result<NewName, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("no name given".into());
    }
    if s.len() > MAX_TYPED_BYTES {
        return Err("that name is too long".into());
    }
    if s.starts_with('/') {
        return Err("give a name relative to the folder shown, not an absolute path".into());
    }
    if let Some(c) = s.chars().find(|c| c.is_control()) {
        return Err(format!("a name cannot contain a control character (U+{:04X})", c as u32));
    }
    let is_dir = s.ends_with('/');
    let mut parts = Vec::new();
    for p in s.split('/') {
        match p {
            "" => {} // `a//b`, and the end of `a/`
            "." | ".." => return Err(format!("'{}' is not allowed in a name", p)),
            p if is_git(p) => return Err(".git belongs to git: ved does not create things in it".into()),
            p if p.len() > MAX_PART_BYTES => return Err(format!("'{}…' is too long for a file name", p.chars().take(20).collect::<String>())),
            p => parts.push(p.to_string()),
        }
    }
    if parts.is_empty() {
        return Err("no name given".into());
    }
    Ok(NewName { parts, is_dir })
}

/// `dir` resolved, and checked to be a folder inside `root`.
fn inside(root: &Path, dir: &Path) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(|e| format!("the project folder: {}", io_msg(&e)))?;
    let dir = dir.canonicalize().map_err(|e| format!("{}: {}", dir.display(), io_msg(&e)))?;
    if !dir.starts_with(&root) {
        return Err("that is outside the project".into());
    }
    if !dir.is_dir() {
        return Err("that is not a folder".into());
    }
    Ok(dir)
}

/// Make the file or folder `name` in the folder `dir` of the project `root`; missing folders on the
/// way are made too. Returns the path (below `dir`, as `dir` was given).
///
/// Nothing that exists is changed: a name that is taken is an error, and so is a link on the way.
pub fn create(root: &Path, dir: &Path, name: &NewName) -> Result<PathBuf, String> {
    let mut cur = inside(root, dir)?;
    let last = name.parts.len() - 1;
    for (i, part) in name.parts.iter().enumerate() {
        let next = cur.join(part);
        match fs::symlink_metadata(&next) {
            Ok(m) if m.file_type().is_symlink() => return Err(format!("{} is a link (ved does not go through links)", part)),
            Ok(m) if i < last && m.is_dir() => {
                cur = next;
                continue;
            }
            Ok(_) if i < last => return Err(format!("{} exists and is not a folder", part)),
            Ok(m) => return Err(format!("{} already exists{}", part, if m.is_dir() { " (a folder)" } else { "" })),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {}", part, io_msg(&e))),
        }
        if i < last || name.is_dir {
            fs::create_dir(&next).map_err(|e| format!("{}: {}", part, io_msg(&e)))?;
        } else {
            // create_new is O_CREAT | O_EXCL: it fails rather than open what appeared meanwhile, a link included
            fs::OpenOptions::new().write(true).create_new(true).open(&next).map_err(|e| format!("{}: {}", part, io_msg(&e)))?;
        }
        cur = next;
    }
    Ok(name.parts.iter().fold(dir.to_path_buf(), |p, part| p.join(part)))
}

/// How many things are inside the folder `path` (not following links), counting at most `limit` and for
/// at most `budget`. The bool says the count is cut short.
pub fn count_below(path: &Path, limit: usize, budget: Duration) -> (usize, bool) {
    let start = Instant::now();
    let mut n = 0usize;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            n += 1;
            if n >= limit || start.elapsed() > budget {
                return (n.min(limit), true);
            }
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(e.path());
            }
        }
    }
    (n, false)
}

/// What is at `path` right now, for a deletion: `Ok(true)` a folder, `Ok(false)` anything else
/// (links included: they are deleted themselves, never followed).
pub fn kind_now(root: &Path, path: &Path) -> Result<bool, String> {
    let target = checked_target(root, path)?;
    let m = fs::symlink_metadata(&target).map_err(|e| if e.kind() == io::ErrorKind::NotFound { "it is already gone".to_string() } else { io_msg(&e) })?;
    Ok(m.is_dir() && !m.file_type().is_symlink())
}

/// `path` with its folder resolved, checked to be strictly inside the project and not in `.git`.
fn checked_target(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let name = path.file_name().ok_or("nothing to delete here")?;
    if path.components().any(|c| is_git(&c.as_os_str().to_string_lossy())) {
        return Err(".git belongs to git: ved does not delete it".into());
    }
    let parent = path.parent().ok_or("nothing to delete here")?;
    // the folder it is in is resolved (a link swapped in for it since the panel was drawn would
    // otherwise send the deletion somewhere else), and it must be inside the project
    let parent = inside(root, parent)?;
    Ok(parent.join(name))
}

/// Delete the file, link or folder (with everything in it) at `path`, which the person was shown as a
/// folder (`expect_dir`) or as something else. Refused if it is not that any more, if it is the
/// project itself, or if it holds the editor's working directory.
pub fn delete(root: &Path, path: &Path, expect_dir: bool) -> Result<(), String> {
    let target = checked_target(root, path)?;
    if target == root.canonicalize().unwrap_or_else(|_| root.to_path_buf()) {
        return Err("that is the project folder itself".into());
    }
    let is_dir = kind_now(root, path)?;
    if is_dir != expect_dir {
        return Err("it changed since the panel was drawn: refresh it (R) and look again".into());
    }
    if is_dir {
        if let Ok(cwd) = std::env::current_dir().and_then(|c| c.canonicalize()) {
            if cwd.starts_with(&target) {
                return Err("the editor's working directory is inside it".into());
            }
        }
        // remove_dir_all does not follow links, and is not fooled by one swapped in during the walk
        fs::remove_dir_all(&target).map_err(|e| io_msg(&e))
    } else {
        fs::remove_file(&target).map_err(|e| io_msg(&e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ved-fileops-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    fn nn(s: &str) -> NewName {
        parse_name(s).unwrap()
    }

    #[test]
    fn names_are_checked_before_anything_is_touched() {
        assert_eq!(nn("a.rs"), NewName { parts: vec!["a.rs".into()], is_dir: false });
        assert_eq!(nn("  src/deep/a.rs  "), NewName { parts: vec!["src".into(), "deep".into(), "a.rs".into()], is_dir: false });
        assert_eq!(nn("dir/"), NewName { parts: vec!["dir".into()], is_dir: true });
        assert_eq!(nn("a//b/"), NewName { parts: vec!["a".into(), "b".into()], is_dir: true });
        assert_eq!(nn("na\u{ef}ve \u{65e5}\u{672c}.txt").parts.len(), 1, "any other text is a fine name");
        assert_eq!(nn("-rf").parts, vec!["-rf"]);
        for bad in ["", "   ", "/", "/etc/passwd", "..", "../x", "a/../b", "a/./b", "./a", "a\u{0}b", "a\nb", "a\u{1b}[2Jb", "a\u{85}b", ".git", "x/.git/hooks/pre-commit", "a/.git", ".GIT", "x/.Git/hooks"] {
            assert!(parse_name(bad).is_err(), "{:?} should be refused", bad);
        }
        assert!(parse_name(&"a".repeat(256)).is_err());
        assert!(parse_name(&"a".repeat(255)).is_ok());
        assert!(parse_name(&format!("{}/x", "a/".repeat(2500))).is_err());
        // a name like .gitignore is not .git
        assert!(parse_name(".gitignore").is_ok());
    }

    #[test]
    fn a_file_is_made_empty_in_the_folder_shown_and_nothing_is_overwritten() {
        let d = scratch("create");
        let p = create(&d, &d, &nn("new.txt")).unwrap();
        assert_eq!(p, d.join("new.txt"));
        assert_eq!(fs::read(&p).unwrap(), b"");
        fs::write(&p, "keep me").unwrap();
        assert!(create(&d, &d, &nn("new.txt")).unwrap_err().contains("already exists"));
        assert_eq!(fs::read_to_string(&p).unwrap(), "keep me", "the existing file was not touched");
        // a name that is a folder
        fs::create_dir(d.join("folder")).unwrap();
        assert!(create(&d, &d, &nn("folder")).unwrap_err().contains("already exists"));
        assert!(create(&d, &d, &nn("folder/")).unwrap_err().contains("already exists"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn folders_on_the_way_are_made_and_a_trailing_slash_makes_a_folder() {
        let d = scratch("nested");
        let p = create(&d, &d, &nn("a/b/c.txt")).unwrap();
        assert!(d.join("a/b").is_dir() && p.is_file());
        let p = create(&d, &d.join("a"), &nn("x/y/")).unwrap();
        assert_eq!(p, d.join("a/x/y"));
        assert!(p.is_dir());
        // through an existing folder
        create(&d, &d, &nn("a/b/d.txt")).unwrap();
        // through a file: no
        assert!(create(&d, &d, &nn("a/b/c.txt/z")).unwrap_err().contains("not a folder"));
        assert!(!d.join("a/b/c.txt/z").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn nothing_is_made_outside_the_project_or_through_a_link() {
        let d = scratch("escape");
        let outside = scratch("escape-outside");
        let proj = d.join("proj");
        fs::create_dir(&proj).unwrap();
        // a folder outside the project
        assert!(create(&proj, &outside, &nn("x")).unwrap_err().contains("outside"));
        assert!(create(&proj, &d, &nn("x")).unwrap_err().contains("outside"));
        // a link to a folder outside: not as the folder, not on the way
        symlink(&outside, proj.join("out")).unwrap();
        assert!(create(&proj, &proj.join("out"), &nn("x")).unwrap_err().contains("outside"));
        assert!(create(&proj, &proj, &nn("out/x")).unwrap_err().contains("link"));
        assert!(create(&proj, &proj, &nn("out/")).unwrap_err().contains("link"));
        assert!(!outside.join("x").exists());
        // a dangling link at the name: O_EXCL / the link check stop it from creating its target
        symlink(outside.join("target-file"), proj.join("dangling")).unwrap();
        assert!(create(&proj, &proj, &nn("dangling")).is_err());
        assert!(!outside.join("target-file").exists(), "the link's target was not created");
        // a link to a file inside
        fs::write(proj.join("real"), "r").unwrap();
        symlink(proj.join("real"), proj.join("alias")).unwrap();
        assert!(create(&proj, &proj, &nn("alias")).is_err());
        assert_eq!(fs::read_to_string(proj.join("real")).unwrap(), "r");
        // a folder that is not there
        assert!(create(&proj, &proj.join("nope"), &nn("x")).is_err());
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn deleting_a_file_a_link_and_a_folder() {
        let d = scratch("delete");
        fs::write(d.join("f.txt"), "x").unwrap();
        delete(&d, &d.join("f.txt"), false).unwrap();
        assert!(!d.join("f.txt").exists());
        assert!(delete(&d, &d.join("f.txt"), false).unwrap_err().contains("gone"));
        // a folder with things in it, links among them
        fs::create_dir_all(d.join("dir/sub")).unwrap();
        fs::write(d.join("dir/sub/a"), "a").unwrap();
        let outside = scratch("delete-outside");
        fs::write(outside.join("precious"), "p").unwrap();
        symlink(&outside, d.join("dir/link-out")).unwrap();
        delete(&d, &d.join("dir"), true).unwrap();
        assert!(!d.join("dir").exists());
        assert_eq!(fs::read_to_string(outside.join("precious")).unwrap(), "p", "what a link led to is never deleted");
        // a link on its own: the link goes, not the target
        symlink(&outside, d.join("l")).unwrap();
        delete(&d, &d.join("l"), false).unwrap();
        assert!(!d.join("l").exists() && outside.join("precious").exists());
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn a_deletion_only_goes_through_for_what_was_shown() {
        let d = scratch("changed");
        // shown as a file, a folder now
        fs::create_dir(d.join("x")).unwrap();
        fs::write(d.join("x/inner"), "i").unwrap();
        assert!(delete(&d, &d.join("x"), false).unwrap_err().contains("changed"));
        assert!(d.join("x/inner").exists());
        // shown as a folder, a link now (to somewhere that must survive)
        let outside = scratch("changed-outside");
        fs::write(outside.join("p"), "p").unwrap();
        fs::remove_dir_all(d.join("x")).unwrap();
        symlink(&outside, d.join("x")).unwrap();
        assert!(delete(&d, &d.join("x"), true).unwrap_err().contains("changed"));
        assert!(outside.join("p").exists() && fs::symlink_metadata(d.join("x")).is_ok());
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn the_project_itself_outside_things_and_git_are_not_deleted() {
        let d = scratch("guard");
        let proj = d.join("proj");
        fs::create_dir_all(proj.join(".git")).unwrap();
        fs::write(d.join("neighbour"), "n").unwrap();
        assert!(delete(&proj, &proj, true).is_err(), "the project itself");
        assert!(delete(&proj, &d.join("neighbour"), false).unwrap_err().contains("outside"));
        assert!(delete(&proj, &proj.join("../neighbour"), false).is_err());
        assert!(delete(&proj, &proj.join(".git"), true).unwrap_err().contains("git"));
        assert!(delete(&proj, &proj.join(".GIT"), true).unwrap_err().contains("git"), "case does not matter on a file system that ignores it");
        assert!(proj.join(".git").exists() && d.join("neighbour").exists());
        // a folder reached through a link that leads out of the project
        let outside = scratch("guard-outside");
        fs::write(outside.join("victim"), "v").unwrap();
        symlink(&outside, proj.join("out")).unwrap();
        assert!(delete(&proj, &proj.join("out/victim"), false).unwrap_err().contains("outside"));
        assert!(outside.join("victim").exists());
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn counting_is_bounded() {
        let d = scratch("count");
        fs::create_dir_all(d.join("a/b")).unwrap();
        for i in 0..30 {
            fs::write(d.join(format!("a/f{}", i)), "x").unwrap();
        }
        symlink(&d, d.join("a/b/loop")).unwrap();
        assert_eq!(count_below(&d, 1000, Duration::from_secs(5)), (33, false), "a, 30 files, b and the link (not followed)");
        assert_eq!(count_below(&d, 10, Duration::from_secs(5)), (10, true));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn io_errors_read_plainly() {
        let e = fs::read("/nonexistent/x").unwrap_err();
        assert_eq!(io_msg(&e), "No such file or directory");
    }
}
