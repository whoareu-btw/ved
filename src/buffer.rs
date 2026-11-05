//! Line-based text buffer with grouped undo/redo.
//!
//! Every modification is expressed as "replace lines [start, start+n) with these
//! lines", which keeps undo records small and makes all editing go through one door.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::sync::atomic::{AtomicUsize, Ordering};

/// (row, col); col counts chars, not bytes.
pub type Pos = (usize, usize);

#[derive(Clone)]
struct LineEdit {
    start: usize,
    old: Vec<Vec<char>>,
    new: Vec<Vec<char>>,
}

#[derive(Clone)]
struct Group {
    edits: Vec<LineEdit>,
    before: Pos,
    after: Pos,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Indent {
    pub use_tabs: bool,
    pub width: usize,
}

const MAX_UNDO: usize = 2000;

static MAX_CHARS: AtomicUsize = AtomicUsize::new(64 << 20);

/// The most characters a buffer may hold (and the largest file that is opened): 64 Mi by default.
/// Commands that would grow a buffer past it (`100000p`, a doubling `:s`) refuse instead of
/// running the machine out of memory. The same figure bounds the memory kept for undo.
pub fn max_chars() -> usize {
    MAX_CHARS.load(Ordering::Relaxed)
}

/// Change the limit (tests use a small one).
pub fn set_max_chars(n: usize) {
    MAX_CHARS.store(n.max(1024), Ordering::Relaxed);
}

fn group_size(g: &Group) -> usize {
    g.edits.iter().map(|e| e.old.iter().chain(e.new.iter()).map(|l| l.len() + 1).sum::<usize>()).sum()
}

pub struct Buffer {
    pub lines: Vec<Vec<char>>,
    pub path: Option<PathBuf>,
    pub modified: bool,
    /// Bumped on every change (used as the LSP document version).
    pub version: i32,
    pub crlf: bool,
    pub final_newline: bool,
    /// Indentation style detected from the file, if it had any.
    pub indent: Option<Indent>,
    undo: Vec<Group>,
    /// Characters held by `undo` (old + new text of every edit), to keep its memory bounded.
    undo_size: usize,
    redo: Vec<Group>,
    open: Option<Group>,
    depth: usize,
    save_point: Option<usize>,
    /// True for an empty file / after deleting every line: serialises as zero bytes, not "\n".
    no_lines: bool,
    /// What the file on disk looked like when we last read or wrote it (see `save_opts`).
    disk: Option<DiskStamp>,
}

/// Enough of a file's metadata to notice that someone else changed it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct DiskStamp {
    mtime_sec: i64,
    mtime_nsec: i64,
    len: u64,
    ino: u64,
}

fn stamp_of(m: &fs::Metadata) -> DiskStamp {
    DiskStamp { mtime_sec: m.mtime(), mtime_nsec: m.mtime_nsec(), len: m.len(), ino: m.ino() }
}

/// Read a file for editing. Only regular files are read (a FIFO, a device such as /dev/zero or a
/// directory would block or never end), and never more than the size limit.
pub fn read_regular_file(path: &Path) -> io::Result<Vec<u8>> {
    // O_NONBLOCK: opening a named pipe nobody writes to would otherwise wait for ever, right here
    let f = fs::OpenOptions::new().read(true).custom_flags(crate::sys::OPEN_UNKNOWN_FLAGS).open(path)?;
    let meta = f.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, if meta.is_dir() { "is a directory" } else { "not a regular file" }));
    }
    let limit = max_chars();
    if meta.len() > limit as u64 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("file is too large ({} MiB limit)", limit >> 20)));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    // the file may grow while we read it
    f.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("file is too large ({} MiB limit)", limit >> 20)));
    }
    Ok(bytes)
}

/// Replace the contents of `path` without ever leaving it half-written: the data goes to a temporary
/// file next to it, is synced, and is renamed over the original (which keeps its permission bits). A
/// full disk or a crash in the middle then costs nothing but the temporary file.
///
/// Falls back to writing in place where a rename would change the file's identity or is not
/// possible: a path that is not a regular file (a device, a named pipe), a file with several hard
/// links, a file owned by someone else, a directory we cannot create files in.
pub fn write_atomically(path: &Path, data: &[u8]) -> io::Result<()> {
    // a symbolic link is followed: it is the file it points to that gets replaced
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let meta = fs::metadata(&target).ok();
    if let Some(m) = &meta {
        if !m.is_file() || m.nlink() > 1 || m.uid() != crate::sys::euid() {
            return fs::write(&target, data);
        }
    }
    let dir = match target.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let name = target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
    let mode = meta.as_ref().map_or(0o666, |m| m.permissions().mode() & 0o7777);
    let mut tmp = None;
    for n in 0..100 {
        let candidate = dir.join(format!(".{}.ved-{}-{}.tmp", name.chars().take(100).collect::<String>(), std::process::id(), n));
        match fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(&candidate) {
            Ok(f) => {
                tmp = Some((candidate, f));
                break;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            // cannot create files here (read-only directory, no permission): write the file itself
            Err(_) => return fs::write(&target, data),
        }
    }
    let Some((tmp_path, mut f)) = tmp else { return fs::write(&target, data) };
    let result = (|| {
        io::Write::write_all(&mut f, data)?;
        if let Some(m) = &meta {
            // the umask may have dropped bits; the original's are the ones to keep
            let _ = f.set_permissions(m.permissions());
        }
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp_path, &target)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    // make the rename itself durable (best effort)
    if let Ok(d) = fs::File::open(&dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// A regular file as text (invalid UTF-8 replaced), within the size limit: for the places that only
/// need to look at a file (`:r`, `:grep`, project detection, previews of language server results).
pub fn read_text(path: &Path) -> io::Result<String> {
    read_regular_file(path).map(|b| String::from_utf8_lossy(&b).into_owned())
}

impl Default for Buffer {
    fn default() -> Buffer {
        Buffer::new()
    }
}

impl Buffer {
    pub fn new() -> Buffer {
        Buffer {
            lines: vec![Vec::new()],
            path: None,
            modified: false,
            version: 0,
            crlf: false,
            final_newline: true,
            indent: None,
            undo: Vec::new(),
            undo_size: 0,
            redo: Vec::new(),
            open: None,
            depth: 0,
            save_point: Some(0),
            no_lines: true,
            disk: None,
        }
    }

    pub fn from_text(text: &str) -> Buffer {
        let mut b = Buffer::new();
        b.crlf = text.contains("\r\n");
        let text = if b.crlf { text.replace("\r\n", "\n") } else { text.to_string() };
        b.final_newline = text.is_empty() || text.ends_with('\n');
        b.no_lines = text.is_empty();
        let body = text.strip_suffix('\n').unwrap_or(&text);
        b.lines = body.split('\n').map(|l| l.chars().collect()).collect();
        if b.lines.is_empty() {
            b.lines.push(Vec::new());
        }
        b.indent = b.detect_indent();
        b
    }

    /// Open a file. A missing file yields an empty buffer that is created on save.
    pub fn open(path: &Path) -> io::Result<Buffer> {
        let mut b = match read_regular_file(path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(s) => Buffer::from_text(&s),
                Err(_) => {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "file is not valid UTF-8"));
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => Buffer::new(),
            Err(e) => return Err(e),
        };
        b.path = Some(path.to_path_buf());
        b.disk = fs::metadata(path).ok().map(|m| stamp_of(&m));
        Ok(b)
    }

    /// Characters in the buffer (a newline counts as one).
    pub fn char_count(&self) -> usize {
        self.lines.iter().map(|l| l.len() + 1).sum()
    }

    fn detect_indent(&self) -> Option<Indent> {
        let mut tabs = 0usize;
        let mut spaces = 0usize;
        let mut min_sp = usize::MAX;
        for l in self.lines.iter().take(400) {
            match l.first() {
                Some('\t') => tabs += 1,
                Some(' ') => {
                    let n = l.iter().take_while(|c| **c == ' ').count();
                    if n < l.len() && n > 1 {
                        spaces += 1;
                        min_sp = min_sp.min(n);
                    }
                }
                _ => {}
            }
        }
        if tabs > spaces {
            Some(Indent { use_tabs: true, width: 4 })
        } else if spaces > 0 {
            let w = if min_sp >= 8 {
                8
            } else if min_sp >= 4 {
                4
            } else {
                2
            };
            Some(Indent { use_tabs: false, width: w })
        } else {
            None
        }
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, r: usize) -> &[char] {
        &self.lines[r]
    }

    pub fn line_len(&self, r: usize) -> usize {
        self.lines[r].len()
    }

    pub fn line_string(&self, r: usize) -> String {
        self.lines[r].iter().collect()
    }

    /// Text with `\n` line endings (what we hand to language servers).
    pub fn text(&self) -> String {
        let mut s = String::new();
        for (i, l) in self.lines.iter().enumerate() {
            if i > 0 {
                s.push('\n');
            }
            s.extend(l.iter());
        }
        let empty = self.no_lines && self.lines.len() == 1 && self.lines[0].is_empty();
        if self.final_newline && !empty {
            s.push('\n');
        }
        s
    }

    pub fn save(&mut self) -> io::Result<usize> {
        self.save_opts(None, false)
    }

    pub fn save_as(&mut self, path: &Path) -> io::Result<usize> {
        self.save_opts(Some(path), false)
    }

    /// Write the buffer to `path` (default: its own file). Unless `force`, refuses to overwrite a file
    /// that changed on disk since we read it, or an existing file that is not the buffer's own.
    pub fn save_opts(&mut self, path: Option<&Path>, force: bool) -> io::Result<usize> {
        let own = self.path.clone();
        let path = match path {
            Some(p) => p.to_path_buf(),
            None => own.clone().ok_or_else(|| io::Error::other("no file name"))?,
        };
        let is_own = own.as_deref() == Some(path.as_path());
        if !force {
            match fs::metadata(&path) {
                Ok(m) if is_own => {
                    if self.disk != Some(stamp_of(&m)) {
                        return Err(io::Error::other("the file changed on disk since it was read (add ! to overwrite)"));
                    }
                }
                Ok(_) => return Err(io::Error::new(io::ErrorKind::AlreadyExists, "file exists (add ! to overwrite)")),
                Err(_) if is_own && self.disk.is_some() => {
                    // it was there when we read it: someone removed it. Writing recreates it; that is fine.
                }
                Err(_) => {}
            }
        }
        let mut s = self.text();
        if self.crlf {
            s = s.replace('\n', "\r\n");
        }
        write_atomically(&path, s.as_bytes())?;
        let stamp = fs::metadata(&path).ok().map(|m| stamp_of(&m));
        if self.path.is_none() {
            self.path = Some(path.clone());
        }
        if self.path.as_deref() == Some(path.as_path()) {
            self.modified = false;
            self.save_point = Some(self.undo.len());
            self.disk = stamp;
        }
        Ok(s.len())
    }

    // ---- undo grouping -------------------------------------------------

    /// Start (or nest into) an undo group; everything until the matching
    /// `end_group` undoes as one step.
    pub fn begin_group(&mut self, cursor: Pos) {
        self.depth += 1;
        if self.depth == 1 {
            self.open = Some(Group { edits: Vec::new(), before: cursor, after: cursor });
        }
    }

    pub fn end_group(&mut self, cursor: Pos) {
        if self.depth == 0 {
            return;
        }
        self.depth -= 1;
        if self.depth == 0 {
            if let Some(mut g) = self.open.take() {
                if !g.edits.is_empty() {
                    g.after = cursor;
                    self.push_undo(g);
                }
            }
        }
    }

    /// Close whatever undo group is open (after an internal error left one dangling).
    pub fn abort_group(&mut self) {
        self.depth = 0;
        if let Some(g) = self.open.take() {
            if !g.edits.is_empty() {
                self.push_undo(g);
            }
        }
    }

    pub fn in_group(&self) -> bool {
        self.depth > 0
    }

    fn push_undo(&mut self, g: Group) {
        if let Some(sp) = self.save_point {
            if sp > self.undo.len() {
                self.save_point = None;
            }
        }
        self.undo_size = self.undo_size.saturating_add(group_size(&g));
        self.undo.push(g);
        // keep the history bounded in count and in memory (the newest change is always kept)
        while self.undo.len() > 1 && (self.undo.len() > MAX_UNDO || self.undo_size > max_chars()) {
            let old = self.undo.remove(0);
            self.undo_size = self.undo_size.saturating_sub(group_size(&old));
            self.save_point = self.save_point.and_then(|s| s.checked_sub(1));
        }
    }

    // ---- the one low-level edit ---------------------------------------

    fn replace_lines(&mut self, start: usize, count: usize, mut new: Vec<Vec<char>>) {
        let start = start.min(self.lines.len());
        let end = (start + count).min(self.lines.len());
        if new.is_empty() && start == 0 && end == self.lines.len() {
            new.push(Vec::new()); // the buffer is never empty
            self.no_lines = true;
        } else {
            self.no_lines = false;
        }
        let old: Vec<Vec<char>> = self.lines.splice(start..end, new.iter().cloned()).collect();
        self.modified = true;
        self.version += 1;
        self.redo.clear();
        let edit = LineEdit { start, old, new };
        if let Some(g) = self.open.as_mut() {
            if let Some(last) = g.edits.last_mut() {
                // Typing within one line: keep only the first "old" and the latest "new".
                if last.start == edit.start
                    && last.new.len() == 1
                    && edit.old.len() == 1
                    && edit.new.len() == 1
                {
                    last.new = edit.new;
                    return;
                }
            }
            g.edits.push(edit);
            return;
        }
        let g = Group { edits: vec![edit], before: (start, 0), after: (start, 0) };
        self.push_undo(g);
    }

    fn apply(&mut self, start: usize, count: usize, with: &[Vec<char>]) {
        let start = start.min(self.lines.len());
        let end = (start + count).min(self.lines.len());
        self.lines.splice(start..end, with.iter().cloned());
        if self.lines.is_empty() {
            self.lines.push(Vec::new());
        }
    }

    pub fn undo(&mut self) -> Option<Pos> {
        let g = self.undo.pop()?;
        self.undo_size = self.undo_size.saturating_sub(group_size(&g));
        for e in g.edits.iter().rev() {
            self.apply(e.start, e.new.len(), &e.old);
        }
        let pos = g.before;
        self.redo.push(g);
        self.version += 1;
        self.modified = self.save_point != Some(self.undo.len());
        Some(pos)
    }

    pub fn redo(&mut self) -> Option<Pos> {
        let g = self.redo.pop()?;
        for e in g.edits.iter() {
            self.apply(e.start, e.old.len(), &e.new);
        }
        let pos = g.before;
        self.undo_size = self.undo_size.saturating_add(group_size(&g));
        self.undo.push(g);
        self.version += 1;
        self.modified = self.save_point != Some(self.undo.len());
        Some(pos)
    }

    // ---- edit helpers ---------------------------------------------------

    pub fn clamp(&self, p: Pos) -> Pos {
        let r = p.0.min(self.lines.len() - 1);
        (r, p.1.min(self.lines[r].len()))
    }

    pub fn set_line(&mut self, r: usize, chars: Vec<char>) {
        self.replace_lines(r, 1, vec![chars]);
    }

    /// Replace whole lines r0..=r1 with `new` (which may be longer or shorter).
    pub fn replace_line_range(&mut self, r0: usize, r1: usize, new: Vec<Vec<char>>) {
        let r1 = r1.min(self.lines.len() - 1);
        if r0 <= r1 {
            self.replace_lines(r0, r1 - r0 + 1, new);
        }
    }

    pub fn insert_lines(&mut self, at_row: usize, lines: Vec<Vec<char>>) {
        if !lines.is_empty() {
            self.replace_lines(at_row, 0, lines);
        }
    }

    /// Delete whole lines r0..=r1.
    pub fn delete_lines(&mut self, r0: usize, r1: usize) {
        let r1 = r1.min(self.lines.len() - 1);
        if r0 <= r1 {
            self.replace_lines(r0, r1 - r0 + 1, Vec::new());
        }
    }

    /// Insert text (may contain `\n`). Returns the position just after it.
    pub fn insert_text(&mut self, at: Pos, text: &[char]) -> Pos {
        if text.is_empty() {
            return at;
        }
        let (r, c) = self.clamp(at);
        let head: Vec<char> = self.lines[r][..c].to_vec();
        let tail: Vec<char> = self.lines[r][c..].to_vec();
        let mut segs: Vec<Vec<char>> = text.split(|ch| *ch == '\n').map(|s| s.to_vec()).collect();
        let n = segs.len();
        let mut first = head;
        first.extend_from_slice(&segs[0]);
        segs[0] = first;
        let end = if n == 1 { (r, segs[0].len()) } else { (r + n - 1, segs[n - 1].len()) };
        segs[n - 1].extend(tail);
        self.replace_lines(r, 1, segs);
        end
    }

    pub fn insert_str(&mut self, at: Pos, s: &str) -> Pos {
        let chars: Vec<char> = s.chars().collect();
        self.insert_text(at, &chars)
    }

    /// Delete the half-open range [a, b) (either order).
    pub fn delete_range(&mut self, a: Pos, b: Pos) {
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        let a = self.clamp(a);
        let b = self.clamp(b);
        if a == b {
            return;
        }
        if a.0 == b.0 {
            let mut nl = self.lines[a.0].clone();
            nl.drain(a.1..b.1);
            self.replace_lines(a.0, 1, vec![nl]);
        } else {
            let mut nl = self.lines[a.0][..a.1].to_vec();
            nl.extend_from_slice(&self.lines[b.0][b.1..]);
            self.replace_lines(a.0, b.0 - a.0 + 1, vec![nl]);
        }
    }

    /// Text in [a, b) with `\n` between lines.
    pub fn get_range(&self, a: Pos, b: Pos) -> Vec<char> {
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        let a = self.clamp(a);
        let b = self.clamp(b);
        if a.0 == b.0 {
            return self.lines[a.0][a.1..b.1].to_vec();
        }
        let mut out: Vec<char> = self.lines[a.0][a.1..].to_vec();
        for r in a.0 + 1..b.0 {
            out.push('\n');
            out.extend_from_slice(&self.lines[r]);
        }
        out.push('\n');
        out.extend_from_slice(&self.lines[b.0][..b.1]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(b: &Buffer) -> String {
        b.text()
    }

    #[test]
    fn a_named_pipe_is_refused_without_blocking() {
        extern "C" {
            fn mkfifo(path: *const std::os::raw::c_char, mode: u32) -> i32;
        }
        let dir = std::env::temp_dir().join(format!("ved-fifo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("pipe");
        let c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        assert_eq!(unsafe { mkfifo(c.as_ptr(), 0o600) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        let p = fifo.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Buffer::open(&p).is_err());
        });
        let refused = rx.recv_timeout(std::time::Duration::from_secs(5));
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(refused, Ok(true), "opening a named pipe must fail at once, not hang");
    }

    #[test]
    fn load_and_roundtrip() {
        let b = Buffer::from_text("a\nb\n");
        assert_eq!(b.lines.len(), 2);
        assert_eq!(text(&b), "a\nb\n");
        let b = Buffer::from_text("a\nb");
        assert!(!b.final_newline);
        assert_eq!(text(&b), "a\nb");
        let b = Buffer::from_text("");
        assert_eq!(b.lines.len(), 1);
        assert_eq!(text(&b), "");
        let b = Buffer::from_text("a\r\nb\r\n");
        assert!(b.crlf);
        assert_eq!(b.lines.len(), 2);
    }

    #[test]
    fn insert_and_delete_multiline() {
        let mut b = Buffer::from_text("hello world\nsecond\n");
        let end = b.insert_str((0, 5), "\nX\nY");
        assert_eq!(end, (2, 1));
        assert_eq!(text(&b), "hello\nX\nY world\nsecond\n");
        b.delete_range((0, 3), (2, 1));
        assert_eq!(text(&b), "hel world\nsecond\n");
        assert_eq!(b.get_range((0, 2), (1, 3)).iter().collect::<String>(), "l world\nsec");
    }

    #[test]
    fn delete_all_lines_keeps_one() {
        let mut b = Buffer::from_text("a\nb\n");
        b.delete_lines(0, 1);
        assert_eq!(b.lines.len(), 1);
        assert!(b.lines[0].is_empty());
    }

    #[test]
    fn undo_redo_groups() {
        let mut b = Buffer::from_text("one\ntwo\n");
        b.begin_group((0, 0));
        b.insert_str((0, 3), "!");
        b.insert_str((0, 4), "?");
        b.insert_str((1, 0), "x\ny\n");
        b.end_group((0, 0));
        assert_eq!(text(&b), "one!?\nx\ny\ntwo\n");
        assert!(b.modified);
        assert!(b.undo().is_some());
        assert_eq!(text(&b), "one\ntwo\n");
        assert!(!b.modified);
        assert!(b.redo().is_some());
        assert_eq!(text(&b), "one!?\nx\ny\ntwo\n");
        assert!(b.undo().is_some());
        assert!(b.undo().is_none());
    }

    #[test]
    fn typing_coalesces_in_a_group() {
        let mut b = Buffer::from_text("\n");
        b.begin_group((0, 0));
        for (i, ch) in "hello".chars().enumerate() {
            b.insert_text((0, i), &[ch]);
        }
        b.end_group((0, 5));
        assert_eq!(b.undo.last().unwrap().edits.len(), 1);
        b.undo();
        assert_eq!(text(&b), "\n"); // the file was one empty line, and stays one
    }

    #[test]
    fn empty_buffer_serialisation() {
        let mut b = Buffer::from_text("");
        assert_eq!(b.text(), "");
        b.insert_str((0, 0), "x");
        assert_eq!(b.text(), "x\n");
        let mut b = Buffer::from_text("a\n");
        b.delete_lines(0, 0);
        assert_eq!(b.text(), ""); // `dd` on the only line: zero bytes, like vim
        let mut b = Buffer::from_text("a\n");
        b.set_line(0, Vec::new());
        assert_eq!(b.text(), "\n"); // `cc`: one empty line
    }

    #[test]
    fn replace_whole_range() {
        let mut b = Buffer::from_text("c\nb\na\n");
        b.replace_line_range(0, 2, vec![vec!['a'], vec!['b'], vec!['c']]);
        assert_eq!(b.text(), "a\nb\nc\n");
        b.undo();
        assert_eq!(b.text(), "c\nb\na\n");
    }

    #[test]
    fn indent_detection() {
        let b = Buffer::from_text("fn x() {\n    a;\n    b;\n}\n");
        assert_eq!(b.indent, Some(Indent { use_tabs: false, width: 4 }));
        let b = Buffer::from_text("f {\n\ta\n\tb\n}\n");
        assert_eq!(b.indent.map(|i| i.use_tabs), Some(true));
    }
}
