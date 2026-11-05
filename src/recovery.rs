//! Saving unsaved work when the editor cannot carry on (an internal error, the terminal being closed,
//! a SIGTERM): every modified buffer is written to a private directory, never over the original file.
//!
//! The directory is `$XDG_STATE_HOME/ved/recovery` (or `~/.local/state/ved/recovery`), falling back to
//! `/tmp/ved-recovery-<uid>`. It must be a real directory (not a symlink) that we own and nobody else
//! can write to; files are created exclusively with mode 0600, so a planted symlink or a file of
//! someone else can neither redirect nor read what we write.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Is `dir` a real directory owned by us that only we can write to? Tightens the mode if we own it.
fn private_dir_ok(dir: &Path) -> bool {
    let Ok(m) = fs::symlink_metadata(dir) else { return false };
    if !m.is_dir() || m.uid() != crate::sys::euid() {
        return false;
    }
    if m.mode() & 0o077 != 0 && fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).is_err() {
        return false;
    }
    true
}

fn make_private_dir(dir: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    if private_dir_ok(dir) {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::PermissionDenied, "recovery directory is not private"))
    }
}

/// Where recovered files go (created if needed).
pub fn dir() -> io::Result<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(x) = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).filter(|p| p.is_absolute()) {
        candidates.push(x.join("ved/recovery"));
    }
    if let Some(h) = std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_absolute()) {
        candidates.push(h.join(".local/state/ved/recovery"));
    }
    candidates.push(PathBuf::from(format!("/tmp/ved-recovery-{}", crate::sys::euid())));
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no place to keep recovery files");
    for c in candidates {
        match make_private_dir(&c) {
            Ok(()) => return Ok(c),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Append a line to the recovery log (best effort): used to keep a record of internal errors.
pub fn note(line: &str) {
    if let Ok(d) = dir() {
        if let Ok(mut f) = OpenOptions::new().append(true).create(true).mode(0o600).open(d.join("recovery.log")) {
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let _ = writeln!(f, "{}\t{}", stamp, line.replace(['\n', '\r'], " "));
        }
    }
}

/// A file name that is safe to create: the original's name with everything odd replaced.
fn safe_name(hint: &str) -> String {
    let base = Path::new(hint).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let mut s: String = base.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' }).take(60).collect();
    if s.is_empty() || s.chars().all(|c| c == '.') {
        s = "unnamed".into();
    }
    s
}

/// Write `data` (the text of the buffer that belonged to `name`) to a new file in the recovery
/// directory and return its path.
pub fn save(name: &str, data: &[u8]) -> io::Result<PathBuf> {
    save_in(&dir()?, name, data)
}

pub fn save_in(dir: &Path, name: &str, data: &[u8]) -> io::Result<PathBuf> {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let base = safe_name(name);
    for n in 0..1000 {
        let file = dir.join(format!("{}-{}-{}{}.recovered", stamp, std::process::id(), base, if n == 0 { String::new() } else { format!("-{}", n) }));
        match OpenOptions::new().write(true).create_new(true).mode(0o600).open(&file) {
            Ok(mut f) => {
                f.write_all(data)?;
                f.sync_all()?;
                // a line saying which file this was (best effort)
                if let Ok(mut log) = OpenOptions::new().append(true).create(true).mode(0o600).open(dir.join("recovery.log")) {
                    let _ = writeln!(log, "{}\t{}\t{}", stamp, file.display(), name.replace(['\n', '\r', '\t'], " "));
                }
                return Ok(file);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "too many recovery files with that name"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ved-recovery-test-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn files_are_private_and_never_overwrite() {
        let d = tmp("a");
        make_private_dir(&d).unwrap();
        assert_eq!(fs::metadata(&d).unwrap().mode() & 0o777, 0o700);
        let a = save_in(&d, "/home/me/notes.txt", b"one").unwrap();
        let b = save_in(&d, "/home/me/notes.txt", b"two").unwrap();
        assert_ne!(a, b);
        assert_eq!(fs::read(&a).unwrap(), b"one");
        assert_eq!(fs::metadata(&a).unwrap().mode() & 0o777, 0o600);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_planted_symlink_is_not_followed() {
        let d = tmp("b");
        make_private_dir(&d).unwrap();
        let victim = tmp("victim");
        fs::write(&victim, b"precious").unwrap();
        // pre-plant the names we would pick for the first few files
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        for t in stamp.saturating_sub(2)..=stamp + 2 {
            let _ = std::os::unix::fs::symlink(&victim, d.join(format!("{}-{}-x.txt.recovered", t, std::process::id())));
        }
        let saved = save_in(&d, "x.txt", b"data").unwrap();
        assert_eq!(fs::read(&victim).unwrap(), b"precious", "the symlink target was written through");
        assert_eq!(fs::read(&saved).unwrap(), b"data");
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_file(&victim);
    }

    #[test]
    fn a_directory_that_is_a_symlink_or_not_ours_is_refused() {
        let real = tmp("c-real");
        fs::create_dir_all(&real).unwrap();
        let link = tmp("c-link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(!private_dir_ok(&link));
        assert!(make_private_dir(&link).is_err());
        let _ = fs::remove_dir_all(&real);
        let _ = fs::remove_file(&link);
    }

    #[test]
    fn hostile_names_become_plain_file_names() {
        assert_eq!(safe_name("/a/b/../../etc/passwd"), "passwd");
        assert_eq!(safe_name("we ird\n\u{1b}[31m.txt"), "we_ird__[31m.txt".replace('[', "_"));
        assert_eq!(safe_name(""), "unnamed");
        assert_eq!(safe_name(".."), "unnamed");
        assert!(safe_name(&"x".repeat(500)).len() <= 60);
    }
}
