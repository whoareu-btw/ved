//! The project panel's data: a lazily listed, bounded file tree with git status.
//!
//! The panel must never be a way to hang or exhaust the editor, so everything here is capped:
//! a directory shows at most `max_entries` names and is listed within a time budget, nothing is
//! followed through a symbolic link, only directories the user opened are read, and `git status`
//! runs on a thread of its own under a time and output limit (a big repository costs a moment of
//! missing colours, not a frozen editor).

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// Names shown for one directory, rows shown in all, and how deep opened directories may nest.
pub const MAX_ENTRIES: usize = 5000;
pub const MAX_ROWS: usize = 50_000;
pub const MAX_DEPTH: usize = 40;
/// How long listing one directory may take (a slow network disk, a directory with millions of names).
const LIST_BUDGET: Duration = Duration::from_millis(200);
const GIT_TIME: Duration = Duration::from_secs(5);
const GIT_OUTPUT: usize = 8 << 20;
const MAX_GIT_ENTRIES: usize = 100_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Dir,
    File,
    /// A symbolic link: shown, opened if it leads to a file, never followed as a directory.
    Link,
    /// A socket, a device, a named pipe...
    Other,
    /// "… more": the directory has more names than are listed.
    More,
}

/// What git says about a path. The order is the order of importance for directories, which show
/// the strongest state found below them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum Git {
    #[default]
    Clean,
    Ignored,
    Added,
    Modified,
    Deleted,
    Conflict,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub kind: Kind,
    pub open: bool,
    pub git: Git,
}

#[derive(Default)]
struct GitReply {
    /// states git reports for exact paths (an untracked or ignored directory stands for all in it)
    direct: HashMap<PathBuf, Git>,
    /// the strongest state below each directory
    dirs: HashMap<PathBuf, Git>,
}

pub struct Tree {
    pub focused: bool,
    /// a first key of a two-key command was typed (`g` of `gg`, Ctrl-w)
    pub pending: Option<char>,
    /// the open file's canonical path (what the panel marks as the current file)
    pub current: Option<PathBuf>,
    pub loaded: bool,
    pub root: PathBuf,
    pub rows: Vec<Row>,
    pub sel: usize,
    pub top: usize,
    /// rows the panel shows (set when it is drawn)
    pub view_h: usize,
    /// the selection moved: the next frame scrolls it into view (the wheel scrolls without it)
    pub follow: bool,
    /// the file the selection last followed (see `Editor::tree_sync`)
    pub revealed_for: Option<PathBuf>,
    pub max_entries: usize,
    open: HashSet<PathBuf>,
    direct: HashMap<PathBuf, Git>,
    dirs: HashMap<PathBuf, Git>,
    git_rx: Option<Receiver<GitReply>>,
    git_again: bool,
}

impl Default for Tree {
    fn default() -> Tree {
        Tree::new()
    }
}

/// One name in a directory listing.
struct Entry {
    name: OsString,
    kind: Kind,
}

/// The names in `dir`: directories first, then the rest, each group in case-insensitive order.
/// `.git` is left out. At most `max` names are read and only for as long as the time budget lasts;
/// the flag says the listing is not complete.
fn list_dir(dir: &Path, max: usize, budget: Duration) -> (Vec<Entry>, bool) {
    let start = Instant::now();
    let Ok(rd) = std::fs::read_dir(dir) else { return (Vec::new(), false) };
    let mut v: Vec<Entry> = Vec::new();
    let mut truncated = false;
    for (i, ent) in rd.enumerate() {
        if v.len() >= max || (i % 64 == 63 && start.elapsed() > budget) {
            truncated = true;
            break;
        }
        let Ok(ent) = ent else { continue };
        let name = ent.file_name();
        if name.as_bytes() == b".git" {
            continue;
        }
        let kind = match ent.file_type() {
            Ok(t) if t.is_dir() => Kind::Dir,
            Ok(t) if t.is_file() => Kind::File,
            Ok(t) if t.is_symlink() => Kind::Link,
            _ => Kind::Other,
        };
        v.push(Entry { name, kind });
    }
    let mut keyed: Vec<(bool, String, Entry)> = v.into_iter().map(|e| (e.kind != Kind::Dir, e.name.to_string_lossy().to_lowercase(), e)).collect();
    keyed.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    (keyed.into_iter().map(|k| k.2).collect(), truncated)
}

fn classify(x: u8, y: u8) -> Git {
    match (x, y) {
        (b'?', b'?') => Git::Added,
        (b'!', b'!') => Git::Ignored,
        (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => Git::Conflict,
        (b'D', _) | (_, b'D') => Git::Deleted,
        (b'A', _) | (_, b'A') => Git::Added,
        _ => Git::Modified,
    }
}

/// Read `git status --porcelain=v1 -z` output. Paths in it are relative to the repository's top
/// directory `top`; only what lies below `root` matters.
fn parse_status(top: &Path, out: &[u8], root: &Path) -> GitReply {
    let mut reply = GitReply::default();
    let mut fields = out.split(|b| *b == 0);
    let mut seen = 0usize;
    while let Some(e) = fields.next() {
        if e.len() < 4 || e[2] != b' ' {
            continue;
        }
        seen += 1;
        if seen > MAX_GIT_ENTRIES {
            break;
        }
        let (x, y) = (e[0], e[1]);
        if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') {
            fields.next(); // a rename or copy is followed by the old name
        }
        let mut p = &e[3..];
        while let Some(rest) = p.strip_suffix(b"/") {
            p = rest;
        }
        if p.is_empty() {
            continue;
        }
        let g = classify(x, y);
        let path = top.join(OsStr::from_bytes(p));
        if !path.starts_with(root) {
            continue;
        }
        reply.direct.insert(path.clone(), g);
        if g != Git::Ignored {
            let mut cur = path.parent();
            while let Some(d) = cur {
                if !d.starts_with(root) {
                    break;
                }
                let slot = reply.dirs.entry(d.to_path_buf()).or_insert(Git::Clean);
                if g > *slot {
                    *slot = g;
                }
                cur = d.parent();
            }
        }
    }
    reply
}

/// The `-c name=` arguments that switch off every content filter the repository's own configuration
/// defines, from the NUL-separated `key\nvalue` records of `git config --null --get-regexp`.
///
/// A filter (`filter.<name>.clean`, `.smudge`, `.process`) is a program, and `git status` runs the
/// clean filter of a file whose size or time stamp differs from the index to compare contents: a
/// hostile `.git/config` could start anything just by being opened. An empty command means "no
/// filter" to git. `None` when a name cannot be written as a `-c` argument (then nothing is asked).
fn filter_overrides(records: &[u8]) -> Option<Vec<String>> {
    let mut args = Vec::new();
    for rec in records.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let key = rec.split(|b| *b == b'\n').next().unwrap_or(&[]);
        let key = std::str::from_utf8(key).ok()?;
        if key.is_empty() || key.contains('=') || key.contains('\0') || !key.starts_with("filter.") {
            return None;
        }
        args.push("-c".to_string());
        args.push(format!("{}=", key));
    }
    Some(args)
}

/// Ask git (on the calling thread; see `Tree::request_git`).
fn git_status(root: &Path) -> GitReply {
    let Some(git) = crate::lsp::which("git") else { return GitReply::default() };
    let run = |pre: &[String], args: &[&str]| -> Option<(Option<i32>, Vec<u8>)> {
        let mut c = std::process::Command::new(&git);
        // a repository's own `core.fsmonitor` names a program to run: never let it
        c.args(["-c", "core.fsmonitor=false"]).args(pre).args(args).current_dir(root).env("GIT_OPTIONAL_LOCKS", "0").env("GIT_TERMINAL_PROMPT", "0");
        let ran = crate::ex::run_command_limited(c, GIT_TIME, GIT_OUTPUT).ok()?;
        Some((ran.status.code(), ran.stdout))
    };
    let ok = |pre: &[String], args: &[&str]| run(pre, args).and_then(|(code, out)| (code == Some(0)).then_some(out));
    let Some(mut top) = ok(&[], &["rev-parse", "--show-toplevel"]) else { return GitReply::default() };
    while matches!(top.last(), Some(b'\n' | b'\r')) {
        top.pop();
    }
    if top.is_empty() {
        return GitReply::default();
    }
    let top = PathBuf::from(OsString::from_vec(top));
    // the filters this repository defines for itself (exit status 1: there are none)
    let filters = match run(&[], &["config", "--local", "--includes", "--null", "--get-regexp", r"^filter\..*\.(clean|smudge|process)$"]) {
        Some((Some(0), out)) => match filter_overrides(&out) {
            Some(a) => a,
            None => return GitReply::default(),
        },
        Some((Some(1), _)) => Vec::new(),
        _ => return GitReply::default(),
    };
    match ok(&filters, &["status", "--porcelain=v1", "-z", "--ignored=matching", "--untracked-files=normal", "--ignore-submodules=all"]) {
        Some(out) => parse_status(&top, &out, root),
        None => GitReply::default(),
    }
}

impl Tree {
    pub fn new() -> Tree {
        Tree {
            focused: false,
            pending: None,
            current: None,
            loaded: false,
            root: PathBuf::from("."),
            rows: Vec::new(),
            sel: 0,
            top: 0,
            view_h: 20,
            follow: true,
            revealed_for: None,
            max_entries: MAX_ENTRIES,
            open: HashSet::new(),
            direct: HashMap::new(),
            dirs: HashMap::new(),
            git_rx: None,
            git_again: false,
        }
    }

    /// The name the top bar shows for the project.
    pub fn root_name(&self) -> String {
        match self.root.file_name() {
            Some(n) => n.to_string_lossy().to_string(),
            None => self.root.to_string_lossy().to_string(),
        }
    }

    /// Forget everything (the working directory changed); the next `load` starts over.
    pub fn reset(&mut self) {
        self.loaded = false;
        self.open.clear();
        self.direct.clear();
        self.dirs.clear();
        self.rows.clear();
        self.sel = 0;
        self.top = 0;
        self.revealed_for = None;
        self.current = None;
        self.git_rx = None;
        self.git_again = false;
    }

    /// Take the working directory as the root, list it and ask git about it.
    pub fn load(&mut self) {
        self.root = std::env::current_dir().ok().and_then(|d| std::fs::canonicalize(d).ok()).unwrap_or_else(|| PathBuf::from("."));
        self.loaded = true;
        self.rebuild();
        self.request_git();
    }

    fn git_of(&self, path: &Path, inherited: Git) -> (Git, Git) {
        let own = self.direct.get(path).copied().or(if matches!(inherited, Git::Ignored | Git::Added) { Some(inherited) } else { None });
        let shown = own.or_else(|| self.dirs.get(path).copied()).unwrap_or(Git::Clean);
        let pass = match own {
            Some(g @ (Git::Ignored | Git::Added)) => g,
            _ => Git::Clean,
        };
        (shown, pass)
    }

    fn flatten(&self, dir: &Path, depth: usize, inherited: Git, out: &mut Vec<Row>) {
        if depth > MAX_DEPTH {
            return;
        }
        let (entries, truncated) = list_dir(dir, self.max_entries, LIST_BUDGET);
        for e in entries {
            if out.len() >= MAX_ROWS {
                return;
            }
            let path = dir.join(&e.name);
            let (git, pass) = self.git_of(&path, inherited);
            let open = e.kind == Kind::Dir && self.open.contains(&path);
            out.push(Row { path: path.clone(), name: e.name.to_string_lossy().to_string(), depth, kind: e.kind, open, git });
            if open {
                self.flatten(&path, depth + 1, pass, out);
            }
        }
        if truncated {
            out.push(Row { path: dir.to_path_buf(), name: "\u{2026} more not shown".into(), depth, kind: Kind::More, open: false, git: Git::Clean });
        }
    }

    /// List the root and every opened directory again, keeping the selection on the same path.
    pub fn rebuild(&mut self) {
        let keep = self.rows.get(self.sel).filter(|r| r.kind != Kind::More).map(|r| r.path.clone());
        let mut rows = Vec::new();
        let root = self.root.clone();
        self.flatten(&root, 0, Git::Clean, &mut rows);
        self.rows = rows;
        let last = self.rows.len().saturating_sub(1);
        self.sel = keep.and_then(|p| self.rows.iter().position(|r| r.path == p && r.kind != Kind::More)).unwrap_or(self.sel.min(last));
        self.top = self.top.min(last);
    }

    /// Ask git for the status of everything below the root, on a thread of its own: the answer is
    /// picked up by `poll`. One question at a time; asking again meanwhile asks once more afterwards.
    pub fn request_git(&mut self) {
        if self.git_rx.is_some() {
            self.git_again = true;
            return;
        }
        let (tx, rx) = channel();
        self.git_rx = Some(rx);
        let root = self.root.clone();
        let spawned = std::thread::Builder::new().name("ved-git".into()).spawn(move || {
            let reply = std::panic::catch_unwind(|| git_status(&root)).unwrap_or_default();
            let _ = tx.send(reply);
            crate::sys::wake();
        });
        if spawned.is_err() {
            self.git_rx = None;
        }
    }

    /// Pick up git's answer if it has come. True when the rows changed.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.git_rx else { return false };
        match rx.try_recv() {
            Ok(r) => {
                self.git_rx = None;
                self.direct = r.direct;
                self.dirs = r.dirs;
                self.rebuild();
                if std::mem::take(&mut self.git_again) {
                    self.request_git();
                }
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.git_rx = None;
                false
            }
        }
    }

    pub fn selected(&self) -> Option<&Row> {
        self.rows.get(self.sel)
    }

    pub fn select(&mut self, i: usize) {
        self.sel = i.min(self.rows.len().saturating_sub(1));
        self.follow = true;
    }

    pub fn move_sel(&mut self, delta: isize) {
        let last = self.rows.len().saturating_sub(1) as isize;
        self.sel = (self.sel as isize + delta).clamp(0, last) as usize;
        self.follow = true;
    }

    /// Tell the tree how many rows the panel has; scrolls the selection into view if it moved.
    pub fn set_view(&mut self, h: usize) {
        self.view_h = h.max(1);
        let max = self.rows.len().saturating_sub(self.view_h);
        self.top = self.top.min(max);
        if std::mem::take(&mut self.follow) {
            self.keep_visible(h);
        }
    }

    /// Scroll so the selected row is on screen, in a panel `h` rows high.
    pub fn keep_visible(&mut self, h: usize) {
        self.view_h = h.max(1);
        let h = self.view_h;
        let last = self.rows.len().saturating_sub(1);
        self.sel = self.sel.min(last);
        if self.sel < self.top {
            self.top = self.sel;
        } else if self.sel >= self.top + h {
            self.top = self.sel + 1 - h;
        }
        self.top = self.top.min(self.rows.len().saturating_sub(h));
    }

    /// Scroll the view without moving the selection (mouse wheel).
    pub fn scroll(&mut self, delta: isize) {
        let max = self.rows.len().saturating_sub(self.view_h.max(1)) as isize;
        self.top = (self.top as isize + delta).clamp(0, max.max(0)) as usize;
        self.follow = false;
    }

    pub fn expand(&mut self, i: usize) {
        if let Some(r) = self.rows.get(i) {
            if r.kind == Kind::Dir && !r.open {
                self.open.insert(r.path.clone());
                self.rebuild_keeping(i);
            }
        }
    }

    pub fn collapse(&mut self, i: usize) {
        if let Some(r) = self.rows.get(i) {
            if r.kind == Kind::Dir && r.open {
                self.open.remove(&r.path);
                // everything below it closes too, but is remembered no more
                let prefix = r.path.clone();
                self.open.retain(|p| !p.starts_with(&prefix));
                self.rebuild_keeping(i);
            }
        }
    }

    pub fn toggle(&mut self, i: usize) {
        match self.rows.get(i) {
            Some(r) if r.kind == Kind::Dir && r.open => self.collapse(i),
            Some(r) if r.kind == Kind::Dir => self.expand(i),
            _ => {}
        }
    }

    fn rebuild_keeping(&mut self, i: usize) {
        self.sel = i;
        self.follow = true;
        self.rebuild();
    }

    /// The row of the directory that holds row `i` (None at the top level).
    pub fn parent_of(&self, i: usize) -> Option<usize> {
        let r = self.rows.get(i)?;
        if r.depth == 0 {
            return None;
        }
        (0..i).rev().find(|&j| self.rows[j].depth + 1 == r.depth && self.rows[j].kind == Kind::Dir)
    }

    /// Open the directories above `file` and select it. False if it is not below the root.
    pub fn reveal(&mut self, file: &Path) -> bool {
        let Ok(rel) = file.strip_prefix(&self.root) else { return false };
        let mut dir = self.root.clone();
        let comps: Vec<_> = rel.components().collect();
        for c in comps.iter().take(comps.len().saturating_sub(1)) {
            dir.push(c);
            self.open.insert(dir.clone());
        }
        self.rebuild();
        match self.rows.iter().position(|r| r.path == file && r.kind != Kind::More) {
            Some(i) => {
                self.sel = i;
                self.follow = true;
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ved-tree-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::canonicalize(&d).unwrap()
    }

    fn tree_at(root: &Path) -> Tree {
        let mut t = Tree::new();
        t.root = root.to_path_buf();
        t.loaded = true;
        t.rebuild();
        t
    }

    fn names(t: &Tree) -> Vec<String> {
        t.rows.iter().map(|r| format!("{}{}", "  ".repeat(r.depth), r.name)).collect()
    }

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "x").unwrap();
    }

    #[test]
    fn directories_come_first_then_files_in_case_insensitive_order() {
        let d = scratch("order");
        for f in ["b.txt", "A.txt", "c.rs", ".hidden"] {
            touch(&d.join(f));
        }
        for s in ["zeta", "Alpha", "src", ".git"] {
            std::fs::create_dir(d.join(s)).unwrap();
        }
        let t = tree_at(&d);
        // `.git` is never shown; everything else is, dot files included
        assert_eq!(names(&t), ["Alpha", "src", "zeta", ".hidden", "A.txt", "b.txt", "c.rs"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn opening_closing_and_finding_the_parent() {
        let d = scratch("fold");
        touch(&d.join("a/b/c.txt"));
        touch(&d.join("a/d.txt"));
        touch(&d.join("z.txt"));
        let mut t = tree_at(&d);
        assert_eq!(names(&t), ["a", "z.txt"]);
        t.expand(0);
        assert_eq!(names(&t), ["a", "  b", "  d.txt", "z.txt"]);
        t.expand(1);
        assert_eq!(names(&t), ["a", "  b", "    c.txt", "  d.txt", "z.txt"]);
        assert_eq!(t.parent_of(2), Some(1));
        assert_eq!(t.parent_of(1), Some(0));
        assert_eq!(t.parent_of(0), None);
        // closing a directory forgets what was open below it
        t.collapse(0);
        assert_eq!(names(&t), ["a", "z.txt"]);
        t.expand(0);
        assert_eq!(names(&t), ["a", "  b", "  d.txt", "z.txt"]);
        // toggling a file does nothing
        t.toggle(3);
        assert_eq!(t.rows.len(), 4);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn reveal_opens_the_ancestors_and_selects_the_file() {
        let d = scratch("reveal");
        touch(&d.join("a/b/c.txt"));
        touch(&d.join("a/b/d.txt"));
        let mut t = tree_at(&d);
        assert!(t.reveal(&d.join("a/b/d.txt")));
        assert_eq!(t.selected().unwrap().name, "d.txt");
        assert_eq!(names(&t), ["a", "  b", "    c.txt", "    d.txt"]);
        // outside the root, or missing: refused, selection kept
        assert!(!t.reveal(Path::new("/etc/passwd")));
        assert!(!t.reveal(&d.join("a/b/missing.txt")));
        assert_eq!(t.selected().unwrap().name, "d.txt");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_huge_directory_is_listed_only_up_to_the_cap() {
        let d = scratch("cap");
        for i in 0..50 {
            touch(&d.join(format!("f{:03}", i)));
        }
        let mut t = Tree::new();
        t.root = d.clone();
        t.max_entries = 10;
        t.rebuild();
        assert_eq!(t.rows.len(), 11);
        assert_eq!(t.rows[10].kind, Kind::More);
        // the notice is not a file: it cannot be opened or kept as a selection across a rebuild
        t.select(10);
        t.rebuild();
        assert!(t.selected().is_some());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn symbolic_links_are_never_followed() {
        let d = scratch("links");
        touch(&d.join("real/file.txt"));
        std::os::unix::fs::symlink(&d, d.join("loop")).unwrap();
        std::os::unix::fs::symlink(d.join("real"), d.join("dirlink")).unwrap();
        std::os::unix::fs::symlink(d.join("real/file.txt"), d.join("filelink")).unwrap();
        std::os::unix::fs::symlink(d.join("missing"), d.join("dangling")).unwrap();
        let mut t = tree_at(&d);
        let kinds: Vec<(String, Kind)> = t.rows.iter().map(|r| (r.name.clone(), r.kind)).collect();
        for (n, k) in &kinds {
            if n != "real" {
                assert_eq!(*k, Kind::Link, "{}", n);
            }
        }
        // asking to open a link does not descend into it, so a loop cannot grow the tree
        for i in 0..t.rows.len() {
            t.expand(i);
        }
        assert_eq!(t.rows.len(), kinds.len() + 1, "only `real` opens: {:?}", names(&t));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn nesting_is_limited() {
        let d = scratch("deep");
        let mut p = d.clone();
        for i in 0..(MAX_DEPTH + 6) {
            p.push(format!("d{}", i));
        }
        std::fs::create_dir_all(&p).unwrap();
        touch(&p.join("leaf.txt"));
        let mut t = tree_at(&d);
        // open everything that can be opened
        for _ in 0..(MAX_DEPTH + 10) {
            let last_dir = t.rows.iter().rposition(|r| r.kind == Kind::Dir && !r.open);
            match last_dir {
                Some(i) => t.expand(i),
                None => break,
            }
        }
        assert!(t.rows.iter().all(|r| r.depth <= MAX_DEPTH + 1));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn git_status_output_is_read_correctly() {
        let top = Path::new("/p");
        let out = b" M src/a.rs\0?? new.txt\0?? newdir/\0!! target/\0A  added.rs\0R  to.rs\0from.rs\0UU both.rs\0 D gone.rs\0";
        let r = parse_status(top, out, top);
        let g = |p: &str| r.direct.get(&PathBuf::from(p)).copied();
        assert_eq!(g("/p/src/a.rs"), Some(Git::Modified));
        assert_eq!(g("/p/new.txt"), Some(Git::Added));
        assert_eq!(g("/p/newdir"), Some(Git::Added));
        assert_eq!(g("/p/target"), Some(Git::Ignored));
        assert_eq!(g("/p/added.rs"), Some(Git::Added));
        assert_eq!(g("/p/to.rs"), Some(Git::Modified));
        assert_eq!(g("/p/from.rs"), None, "the old name of a rename is not a path of its own");
        assert_eq!(g("/p/both.rs"), Some(Git::Conflict));
        assert_eq!(g("/p/gone.rs"), Some(Git::Deleted));
        // a directory shows the strongest thing below it; ignored paths colour no parent
        assert_eq!(r.dirs.get(&PathBuf::from("/p/src")), Some(&Git::Modified));
        assert_eq!(r.dirs.get(&PathBuf::from("/p")), Some(&Git::Conflict));
        // garbage and short records are skipped
        let r = parse_status(top, b"\0x\0 M\0??\0\0?? ok.txt\0", top);
        assert_eq!(r.direct.len(), 1);
        // anything outside the panel's root is ignored
        let r = parse_status(top, b" M other/x.rs\0 M in/x.rs\0", &top.join("in"));
        assert_eq!(r.direct.len(), 1);
    }

    #[test]
    fn untracked_and_ignored_directories_colour_what_is_inside() {
        let d = scratch("inherit");
        touch(&d.join("target/debug/x"));
        touch(&d.join("src/m.rs"));
        let mut t = Tree::new();
        t.root = d.clone();
        t.direct.insert(d.join("target"), Git::Ignored);
        t.direct.insert(d.join("src/m.rs"), Git::Modified);
        t.dirs.insert(d.join("src"), Git::Modified);
        t.dirs.insert(d.clone(), Git::Modified);
        t.open.insert(d.join("target"));
        t.open.insert(d.join("target/debug"));
        t.open.insert(d.join("src"));
        t.rebuild();
        let state = |n: &str| t.rows.iter().find(|r| r.name == n).unwrap().git;
        assert_eq!(state("target"), Git::Ignored);
        assert_eq!(state("debug"), Git::Ignored);
        assert_eq!(state("x"), Git::Ignored);
        assert_eq!(state("src"), Git::Modified);
        assert_eq!(state("m.rs"), Git::Modified);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn git_colours_come_from_the_real_git() {
        let Some(git) = crate::lsp::which("git") else { return };
        let d = scratch("realgit");
        let run = |args: &[&str]| {
            let ok = std::process::Command::new(&git)
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&d)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            assert!(ok, "git {:?}", args);
        };
        run(&["init", "-q"]);
        touch(&d.join("kept.txt"));
        touch(&d.join("changed.txt"));
        touch(&d.join(".gitignore"));
        std::fs::write(d.join(".gitignore"), "out/\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "first"]);
        std::fs::write(d.join("changed.txt"), "different").unwrap();
        touch(&d.join("fresh.txt"));
        touch(&d.join("out/o.bin"));
        touch(&d.join("sub/new.txt"));
        let reply = git_status(&d);
        let mut t = Tree::new();
        t.root = d.clone();
        t.direct = reply.direct;
        t.dirs = reply.dirs;
        t.rebuild();
        let state = |n: &str| t.rows.iter().find(|r| r.name == n).map(|r| r.git);
        assert_eq!(state("changed.txt"), Some(Git::Modified));
        assert_eq!(state("fresh.txt"), Some(Git::Added));
        assert_eq!(state("kept.txt"), Some(Git::Clean));
        assert_eq!(state("out"), Some(Git::Ignored));
        assert_eq!(state("sub"), Some(Git::Added));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn filter_drivers_of_the_repository_are_switched_off() {
        let recs = b"filter.evil.clean\nsh -c 'touch x'\0filter.lfs.smudge\ngit-lfs smudge\0filter.a.b.process\0";
        assert_eq!(
            filter_overrides(recs),
            Some(vec!["-c".to_string(), "filter.evil.clean=".into(), "-c".into(), "filter.lfs.smudge=".into(), "-c".into(), "filter.a.b.process=".into()])
        );
        assert_eq!(filter_overrides(b""), Some(Vec::new()));
        // a name that cannot be passed as `-c name=` (an `=` in it) or is not even UTF-8: ask nothing
        assert_eq!(filter_overrides(b"filter.x=y.clean\nv\0"), None);
        assert_eq!(filter_overrides(b"filter.\xff.clean\nv\0"), None);
        assert_eq!(filter_overrides(b"other.x.clean\nv\0"), None);
    }

    /// The real thing: a repository whose own config names a clean filter that leaves a marker.
    #[test]
    fn a_hostile_repository_cannot_run_a_program_through_the_panels_git_status() {
        let Some(git) = crate::lsp::which("git") else { return };
        let d = scratch("hostile");
        let marker = d.join("PWNED");
        let run = |args: &[&str]| {
            let ok = std::process::Command::new(&git)
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&d)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            assert!(ok, "git {:?}", args);
        };
        run(&["init", "-q"]);
        std::fs::write(d.join(".gitattributes"), "* filter=evil\n").unwrap();
        std::fs::write(d.join("f.txt"), "hi\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "x"]);
        run(&["config", "filter.evil.clean", &format!("sh -c 'touch {}; cat'", marker.display())]);
        // a later time stamp makes git compare the contents (and so run the filter), unless it is stopped
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(d.join("f.txt"), "hi\n").unwrap();
        let _ = std::fs::remove_file(&marker);
        let _ = git_status(&d);
        assert!(!marker.exists(), "the repository's own filter program was run");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn git_answers_arrive_through_poll_without_blocking() {
        let d = scratch("poll");
        touch(&d.join("a.txt"));
        let mut t = Tree::new();
        t.root = d.clone();
        t.loaded = true;
        t.rebuild();
        t.request_git();
        t.request_git(); // a second question while one is out is remembered, not run in parallel
        let end = Instant::now() + Duration::from_secs(20);
        let mut got = false;
        while Instant::now() < end && !got {
            got = t.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(got, "no answer from the git thread");
        let _ = std::fs::remove_dir_all(&d);
    }
}
