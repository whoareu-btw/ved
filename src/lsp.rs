//! Built-in Language Server Protocol client.
//!
//! Servers are started automatically per file type (see `LANGS`, overridable with
//! `lsp <ext> <command>` in the config). Each server has a reader thread that parses
//! framed JSON-RPC messages and wakes the event loop; everything else happens on the
//! main thread inside `Editor::tick`.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::buffer::Pos;
use crate::editor::{fuzzy_score, Completion, Editor, Mode, PickItem, PickTarget, Picker, Popup};
use crate::json::{obj, Json};
use crate::motion::is_word_char;
use crate::sys;

// ---------------------------------------------------------------------------------------
// data shared with the rest of the editor

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub start: Pos,
    pub end: Pos,
    /// 1 error, 2 warning, 3 info, 4 hint
    pub severity: u8,
    pub message: String,
    pub source: String,
}

#[derive(Default)]
pub struct DocLsp {
    pub client: Option<usize>,
    pub opened: bool,
    pub uri: String,
    pub path: PathBuf,
    pub sent_version: i32,
    pub dirty_since: Option<Instant>,
    /// the text the server currently believes the document contains
    pub sent: Vec<Vec<char>>,
    /// whether that text ends with a newline (`Buffer::text` adds one only if the file had one)
    pub sent_final_newline: bool,
}

#[derive(Clone, Debug, Default)]
pub struct CompItem {
    pub label: String,
    pub insert: String,
    pub detail: String,
    pub kind: u8,
    pub filter: String,
    pub sort: String,
    /// column (on the cursor line) where the replaced text starts, when the server said so
    pub replace_from: Option<usize>,
    /// extra edits such as auto-imports (raw LSP TextEdits)
    pub extra: Vec<Json>,
    /// the raw LSP snippet (`foo(${1:a})$0`) when the item is one; `insert` then holds its plain text
    pub snippet: Option<String>,
}

fn num(v: usize) -> Json {
    Json::Num(v as f64)
}

fn int(v: i64) -> Json {
    Json::Num(v as f64)
}

fn pos_json(line: usize, ch: usize) -> Json {
    obj(vec![("line", num(line)), ("character", num(ch))])
}

// ---------------------------------------------------------------------------------------
// language table

struct LangDef {
    exts: &'static [&'static str],
    cmds: &'static [&'static str],
}

const LANGS: &[LangDef] = &[
    LangDef { exts: &["rs"], cmds: &["rust-analyzer"] },
    LangDef { exts: &["py", "pyi"], cmds: &["pyright-langserver --stdio", "basedpyright-langserver --stdio", "pylsp", "jedi-language-server", "ruff server"] },
    LangDef { exts: &["js", "jsx", "mjs", "cjs", "ts", "tsx"], cmds: &["typescript-language-server --stdio", "vtsls --stdio"] },
    LangDef { exts: &["c", "h", "cc", "cpp", "cxx", "hpp", "hh", "hxx"], cmds: &["clangd", "ccls"] },
    LangDef { exts: &["go"], cmds: &["gopls"] },
    LangDef { exts: &["lua"], cmds: &["lua-language-server"] },
    LangDef { exts: &["zig"], cmds: &["zls"] },
    LangDef { exts: &["sh", "bash"], cmds: &["bash-language-server start"] },
    LangDef { exts: &["java"], cmds: &["jdtls"] },
    LangDef { exts: &["rb"], cmds: &["solargraph stdio", "ruby-lsp"] },
    LangDef { exts: &["php"], cmds: &["intelephense --stdio", "phpactor language-server"] },
    LangDef { exts: &["json", "jsonc"], cmds: &["vscode-json-language-server --stdio"] },
    LangDef { exts: &["html", "htm"], cmds: &["vscode-html-language-server --stdio"] },
    LangDef { exts: &["css", "scss", "less"], cmds: &["vscode-css-language-server --stdio"] },
    LangDef { exts: &["yml", "yaml"], cmds: &["yaml-language-server --stdio"] },
    LangDef { exts: &["toml"], cmds: &["taplo lsp stdio"] },
    LangDef { exts: &["md", "markdown"], cmds: &["marksman server"] },
    LangDef { exts: &["nix"], cmds: &["nil", "nixd"] },
    LangDef { exts: &["ex", "exs"], cmds: &["elixir-ls"] },
    LangDef { exts: &["swift"], cmds: &["sourcekit-lsp"] },
    LangDef { exts: &["kt", "kts"], cmds: &["kotlin-language-server"] },
];

pub fn language_id(ext: &str) -> &'static str {
    match ext {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "ts" => "typescript",
        "tsx" => "typescriptreact",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "go" => "go",
        "lua" => "lua",
        "zig" => "zig",
        "sh" | "bash" => "shellscript",
        "java" => "java",
        "rb" => "ruby",
        "php" => "php",
        "json" => "json",
        "jsonc" => "jsonc",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "less" => "less",
        "yml" | "yaml" => "yaml",
        "toml" => "toml",
        "md" | "markdown" => "markdown",
        "nix" => "nix",
        "ex" | "exs" => "elixir",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        _ => "plaintext",
    }
}

/// Look a program up in PATH (plus a few places PATH often misses).
pub fn which(prog: &str) -> Option<PathBuf> {
    which_in(prog, std::env::var_os("PATH"), std::env::var("HOME").ok())
}

fn which_in(prog: &str, path: Option<std::ffi::OsString>, home: Option<String>) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    if prog.contains('/') {
        let p = PathBuf::from(prog);
        return if p.is_file() { Some(p) } else { None };
    }
    // An empty or relative PATH entry means "the current directory": a file in the project being
    // edited could then be started as a language server just by opening the project. Skip those.
    let mut dirs: Vec<PathBuf> = path.map(|p| std::env::split_paths(&p).filter(|d| d.is_absolute()).collect()).unwrap_or_default();
    if let Some(h) = home.filter(|h| Path::new(h).is_absolute()) {
        for extra in [".cargo/bin", ".local/bin", "go/bin", ".npm-global/bin", ".bun/bin"] {
            dirs.push(PathBuf::from(&h).join(extra));
        }
    }
    dirs.push(PathBuf::from("/usr/local/bin"));
    for d in dirs {
        let c = d.join(prog);
        if let Ok(m) = std::fs::metadata(&c) {
            if m.is_file() && m.permissions().mode() & 0o111 != 0 {
                return Some(c);
            }
        }
    }
    None
}

const ROOT_MARKERS: &[&str] = &[
    "Cargo.toml", "go.mod", "package.json", "tsconfig.json", "jsconfig.json", "pyproject.toml", "setup.py", "setup.cfg", "pyrightconfig.json",
    "requirements.txt", "compile_commands.json", "compile_flags.txt", ".clangd", "build.zig", ".luarc.json", "pom.xml", "build.gradle",
    "Gemfile", "composer.json", "flake.nix", "mix.exs", ".git",
];

pub fn find_root(file: &Path, ext: &str) -> PathBuf {
    let start = file.parent().unwrap_or(Path::new("/")).to_path_buf();
    let mut found: Option<PathBuf> = None;
    let mut cur = Some(start.as_path());
    while let Some(d) = cur {
        if ext == "rs" {
            // prefer the outermost Cargo workspace
            if let Ok(t) = crate::buffer::read_text(&d.join("Cargo.toml")) {
                if t.contains("[workspace]") {
                    return d.to_path_buf();
                }
                if found.is_none() {
                    found = Some(d.to_path_buf());
                }
            }
        } else if ROOT_MARKERS.iter().any(|m| d.join(m).exists()) {
            return d.to_path_buf();
        }
        cur = d.parent();
    }
    found.unwrap_or(start)
}

// ---------------------------------------------------------------------------------------
// URIs, UTF-16 columns, snippets

pub fn path_to_uri(p: &Path) -> String {
    let mut s = String::from("file://");
    for b in p.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => s.push(b as char),
            _ => s.push_str(&format!("%{:02X}", b)),
        }
    }
    s
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let b = rest.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    Some(PathBuf::from(String::from_utf8_lossy(&out).to_string()))
}

pub fn to_utf16_col(line: &[char], col: usize) -> usize {
    line.iter().take(col).map(|c| c.len_utf16()).sum()
}

pub fn from_utf16_col(line: &[char], u: usize) -> usize {
    let mut acc = 0;
    for (i, c) in line.iter().enumerate() {
        if acc >= u {
            return i;
        }
        acc += c.len_utf16();
    }
    line.len()
}

fn lsp_pos(lines: &[Vec<char>], p: Pos) -> Json {
    let row = p.0.min(lines.len().saturating_sub(1));
    let col = lines.get(row).map_or(0, |l| to_utf16_col(l, p.1));
    pos_json(row, col)
}

fn pos_from_json(lines: &[Vec<char>], j: &Json) -> Pos {
    let row = j.get("line").as_usize().unwrap_or(0);
    let ch = j.get("character").as_usize().unwrap_or(0);
    if row >= lines.len() {
        let last = lines.len().saturating_sub(1);
        return (last, lines.get(last).map_or(0, |l| l.len()));
    }
    (row, from_utf16_col(&lines[row], ch))
}

/// A server's range as ordered positions inside the buffer (servers are not always careful: an
/// inverted or out-of-range range must not turn into a panic or a corrupted buffer).
fn range_from_json(lines: &[Vec<char>], r: &Json) -> (Pos, Pos) {
    let a = pos_from_json(lines, r.get("start"));
    let b = pos_from_json(lines, r.get("end"));
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// The smallest whole-line replacement turning `old` into `new`, as an LSP content change.
/// Only valid for documents that end in a newline (so "line N, column 0" always exists).
fn line_change(old: &[Vec<char>], new: &[Vec<char>]) -> Json {
    let mut a = 0;
    while a < old.len() && a < new.len() && old[a] == new[a] {
        a += 1;
    }
    let mut s = 0;
    while s < old.len() - a && s < new.len() - a && old[old.len() - 1 - s] == new[new.len() - 1 - s] {
        s += 1;
    }
    let eo = old.len() - s;
    let en = new.len() - s;
    let mut text = String::new();
    for l in &new[a..en] {
        text.extend(l.iter());
        text.push('\n');
    }
    obj(vec![("range", obj(vec![("start", pos_json(a, 0)), ("end", pos_json(eo, 0))])), ("text", text.into())])
}

/// The first `max` bytes of a regular file as text (for previews of files a server pointed at).
fn read_prefix(path: &Path, max: u64) -> std::io::Result<String> {
    use std::os::unix::fs::OpenOptionsExt;
    // never blocks on a named pipe a server (or a project) put in the way
    let f = std::fs::OpenOptions::new().read(true).custom_flags(crate::sys::OPEN_UNKNOWN_FLAGS).open(path)?;
    if !f.metadata()?.is_file() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"));
    }
    let mut bytes = Vec::new();
    f.take(max).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn trivial(lines: &[Vec<char>]) -> bool {
    lines.len() == 1 && lines[0].is_empty()
}

// ---------------------------------------------------------------------------------------
// transport

pub enum Msg {
    Json(Json),
    Closed,
}

/// The largest message we accept from a server (a bogus `Content-Length` must not make us allocate
/// gigabytes), the longest header line, and how many messages may wait for the main thread: a
/// server that floods us is slowed down by its full pipe instead of eating our memory.
const MAX_MESSAGE: usize = 64 << 20;
const MAX_HEADER_LINE: usize = 8 << 10;
const INBOX_DEPTH: usize = 4096;
/// Bytes we are willing to keep queued for a server that does not read its input.
const MAX_QUEUED: usize = 64 << 20;

enum Frame {
    Body(Vec<u8>),
    /// a header block without a usable Content-Length: nothing to deliver
    Skip,
    /// end of input, a read error or a protocol error we cannot recover from
    End,
}

/// One header line without its line ending; `None` at end of input or for a line that is too long.
fn read_header_line<R: BufRead>(r: &mut R) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    let n = (&mut *r).take(MAX_HEADER_LINE as u64).read_until(b'\n', &mut buf).ok()?;
    if n == 0 || buf.last() != Some(&b'\n') {
        return None;
    }
    while matches!(buf.last(), Some(b'\n' | b'\r')) {
        buf.pop();
    }
    Some(buf)
}

fn read_frame<R: BufRead>(r: &mut R) -> Frame {
    let mut len: Option<usize> = None;
    loop {
        let Some(line) = read_header_line(r) else { return Frame::End };
        if line.is_empty() {
            break;
        }
        let text = String::from_utf8_lossy(&line);
        if let Some((k, v)) = text.split_once(':') {
            // header names are case-insensitive
            if k.trim().eq_ignore_ascii_case("content-length") {
                len = v.trim().parse().ok();
            }
        }
    }
    let Some(n) = len else { return Frame::Skip };
    if n > MAX_MESSAGE {
        return Frame::End;
    }
    let mut body = Vec::new();
    match (&mut *r).take(n as u64).read_to_end(&mut body) {
        Ok(got) if got == n => Frame::Body(body),
        _ => Frame::End,
    }
}

fn spawn_reader(stdout: impl Read + Send + 'static, tx: SyncSender<Msg>) {
    std::thread::spawn(move || {
        let tx2 = tx.clone();
        // a bug in here must never take the editor down: it ends this server's connection instead
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let mut r = BufReader::new(stdout);
            loop {
                match read_frame(&mut r) {
                    Frame::End => break,
                    Frame::Skip => continue,
                    Frame::Body(body) => {
                        let s = String::from_utf8_lossy(&body);
                        if let Ok(j) = Json::parse(&s) {
                            if tx.send(Msg::Json(j)).is_err() {
                                return;
                            }
                            sys::wake();
                        }
                    }
                }
            }
        }));
        let _ = tx2.send(Msg::Closed);
        sys::wake();
    });
}

/// Writes to the server's stdin from its own thread, so that a server that stops reading can never
/// freeze the editor. `queued` counts the bytes still waiting.
fn spawn_writer(mut stdin: ChildStdin, closed: SyncSender<Msg>, queued: Arc<AtomicUsize>) -> Sender<Vec<u8>> {
    let (tx, rx) = channel::<Vec<u8>>();
    std::thread::spawn(move || {
        while let Ok(buf) = rx.recv() {
            let ok = stdin.write_all(&buf).and_then(|_| stdin.flush()).is_ok();
            queued.fetch_sub(buf.len(), Ordering::Relaxed);
            if !ok {
                let _ = closed.send(Msg::Closed);
                sys::wake();
                break;
            }
        }
        // `stdin` is dropped here: the server sees end of input
    });
    tx
}

/// Keep draining a server's stderr (so it can never block on a full pipe) and remember the tail,
/// which is the best explanation available when a server dies.
fn spawn_stderr_reader(stderr: impl Read + Send + 'static, log: Arc<Mutex<VecDeque<String>>>) {
    std::thread::spawn(move || {
        let mut r = BufReader::new(stderr);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match r.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = String::from_utf8_lossy(&buf).trim().to_string();
            if line.is_empty() {
                continue;
            }
            if let Ok(mut l) = log.lock() {
                if l.len() >= 20 {
                    l.pop_front();
                }
                l.push_back(line.chars().take(300).collect());
            }
        }
    });
}

#[derive(Clone, Debug)]
pub enum Pending {
    Initialize,
    Hover,
    Definition,
    References,
    Completion { uri: String, row: usize, seq: u64 },
    /// the version of every open document when the request was made (see `apply_workspace_edit`)
    Rename { versions: Vec<(PathBuf, i32)> },
    Formatting { uri: String, version: i32 },
    Shutdown,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SState {
    Starting,
    Ready,
    Dead(String),
}

pub struct Server {
    pub name: String,
    pub cmdline: String,
    pub root: PathBuf,
    child: Child,
    /// the process has been waited for: its number may belong to someone else now
    reaped: bool,
    /// to the writer thread; `None` once we stopped talking to the server
    writer: Option<Sender<Vec<u8>>>,
    queued: Arc<AtomicUsize>,
    rx: Receiver<Msg>,
    next_id: i64,
    pub pending: HashMap<i64, Pending>,
    pub state: SState,
    pub caps: Json,
    /// 0 none, 1 full, 2 incremental
    pub sync_kind: u8,
    queue: Vec<Json>,
    pub triggers: Vec<char>,
    pub progress: BTreeMap<String, String>,
    log: Arc<Mutex<VecDeque<String>>>,
    /// when we last re-sent every document because the server lost track of one
    last_resync: Option<Instant>,
}

impl Server {
    pub fn start(cmdline: &str, root: &Path) -> Result<Server, String> {
        let mut parts = cmdline.split_whitespace();
        let prog = parts.next().ok_or("empty language server command")?;
        let path = which(prog).ok_or_else(|| format!("{} not found in PATH", prog))?;
        let mut child = Command::new(path)
            .args(parts)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // its own process group: when we stop the server, whatever it started goes with it
            .process_group(0)
            .spawn()
            .map_err(|e| format!("cannot start {}: {}", prog, e))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let (tx, rx) = sync_channel(INBOX_DEPTH);
        spawn_reader(stdout, tx.clone());
        let queued = Arc::new(AtomicUsize::new(0));
        let writer = stdin.map(|w| spawn_writer(w, tx, queued.clone()));
        let log = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(e) = child.stderr.take() {
            spawn_stderr_reader(e, log.clone());
        }
        let name = Path::new(prog).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| prog.to_string());
        let mut s = Server {
            name,
            cmdline: cmdline.to_string(),
            root: root.to_path_buf(),
            child,
            reaped: false,
            writer,
            queued,
            rx,
            next_id: 1,
            pending: HashMap::new(),
            state: SState::Starting,
            caps: Json::Null,
            sync_kind: 1,
            queue: Vec::new(),
            triggers: Vec::new(),
            progress: BTreeMap::new(),
            log,
            last_resync: None,
        };
        let root_uri = path_to_uri(root);
        let folder_name = root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let caps = obj(vec![
            (
                "textDocument",
                obj(vec![
                    ("synchronization", obj(vec![("dynamicRegistration", false.into()), ("didSave", true.into()), ("willSave", false.into())])),
                    (
                        "completion",
                        obj(vec![
                            ("contextSupport", true.into()),
                            (
                                "completionItem",
                                obj(vec![
                                    ("snippetSupport", true.into()),
                                    ("documentationFormat", Json::Arr(vec!["plaintext".into()])),
                                    ("additionalTextEditsSupport", true.into()),
                                ]),
                            ),
                        ]),
                    ),
                    ("hover", obj(vec![("contentFormat", Json::Arr(vec!["plaintext".into(), "markdown".into()]))])),
                    ("definition", obj(vec![("linkSupport", true.into())])),
                    ("references", obj(vec![])),
                    ("rename", obj(vec![("prepareSupport", false.into())])),
                    ("formatting", obj(vec![])),
                    ("publishDiagnostics", obj(vec![("relatedInformation", false.into())])),
                ]),
            ),
            (
                "workspace",
                obj(vec![
                    ("workspaceFolders", true.into()),
                    ("configuration", true.into()),
                    ("applyEdit", true.into()),
                    ("workspaceEdit", obj(vec![("documentChanges", true.into())])),
                ]),
            ),
            ("window", obj(vec![("workDoneProgress", true.into())])),
            ("general", obj(vec![("positionEncodings", Json::Arr(vec!["utf-16".into()]))])),
        ]);
        let params = obj(vec![
            ("processId", int(std::process::id() as i64)),
            ("clientInfo", obj(vec![("name", "ved".into()), ("version", env!("CARGO_PKG_VERSION").into())])),
            ("rootUri", root_uri.clone().into()),
            ("rootPath", root.to_string_lossy().to_string().into()),
            ("workspaceFolders", Json::Arr(vec![obj(vec![("uri", root_uri.into()), ("name", folder_name.into())])])),
            ("capabilities", caps),
        ]);
        s.request("initialize", params, Pending::Initialize);
        Ok(s)
    }

    pub fn alive(&self) -> bool {
        !matches!(self.state, SState::Dead(_))
    }

    /// The last lines the server wrote to stderr (oldest first).
    pub fn stderr_tail(&self, n: usize) -> Vec<String> {
        match self.log.lock() {
            Ok(l) => l.iter().rev().take(n).rev().cloned().collect(),
            Err(_) => Vec::new(),
        }
    }

    fn write_msg(&mut self, v: &Json) {
        let body = v.to_string();
        let msg = format!("Content-Length: {}\r\n\r\n{}", body.len(), body).into_bytes();
        let Some(w) = self.writer.as_ref() else { return };
        if self.queued.load(Ordering::Relaxed).saturating_add(msg.len()) > MAX_QUEUED {
            // the server is not reading what we send: give up on it rather than pile up memory
            self.kill();
            self.state = SState::Dead("not reading its input".into());
            return;
        }
        self.queued.fetch_add(msg.len(), Ordering::Relaxed);
        if w.send(msg).is_err() {
            // The writer thread has stopped, which it does only after a failed write (the server
            // is gone) and after telling the main thread so: leave the state alone, so that this
            // is handled as the crash it is (message, restart) when that notice is read.
            self.writer = None;
        }
    }

    pub fn request(&mut self, method: &str, params: Json, p: Pending) -> Option<i64> {
        if self.state != SState::Ready && !matches!(p, Pending::Initialize) {
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, p);
        let m = obj(vec![("jsonrpc", "2.0".into()), ("id", int(id)), ("method", method.into()), ("params", params)]);
        self.write_msg(&m);
        Some(id)
    }

    pub fn notify(&mut self, method: &str, params: Json) {
        let m = obj(vec![("jsonrpc", "2.0".into()), ("method", method.into()), ("params", params)]);
        match self.state {
            SState::Ready => self.write_msg(&m),
            SState::Starting => self.queue.push(m),
            SState::Dead(_) => {}
        }
    }

    fn respond(&mut self, id: &Json, result: Json) {
        let m = obj(vec![("jsonrpc", "2.0".into()), ("id", id.clone()), ("result", result)]);
        self.write_msg(&m);
    }

    fn respond_error(&mut self, id: &Json, code: i64, msg: &str) {
        let e = obj(vec![("code", int(code)), ("message", msg.into())]);
        let m = obj(vec![("jsonrpc", "2.0".into()), ("id", id.clone()), ("error", e)]);
        self.write_msg(&m);
    }

    fn become_ready(&mut self, result: &Json) {
        self.caps = result.get("capabilities").clone();
        let sync = self.caps.get("textDocumentSync");
        self.sync_kind = match sync {
            Json::Num(n) => (*n as i64).clamp(0, 2) as u8,
            Json::Obj(_) => sync.get("change").as_i64().unwrap_or(1).clamp(0, 2) as u8,
            _ => 1,
        };
        self.triggers = self
            .caps
            .get("completionProvider")
            .get("triggerCharacters")
            .arr()
            .iter()
            .filter_map(|c| c.as_str().and_then(|s| s.chars().next()))
            .collect();
        self.state = SState::Ready;
        self.write_msg(&obj(vec![("jsonrpc", "2.0".into()), ("method", "initialized".into()), ("params", obj(vec![]))]));
        let q = std::mem::take(&mut self.queue);
        for m in q {
            self.write_msg(&m);
        }
    }

    pub fn supports(&self, cap: &str) -> bool {
        !matches!(self.caps.get(cap), Json::Null | Json::Bool(false))
    }

    /// Ask the server to exit (without waiting).
    fn begin_shutdown(&mut self) {
        if self.state == SState::Ready {
            self.request("shutdown", Json::Null, Pending::Shutdown);
            self.write_msg(&obj(vec![("jsonrpc", "2.0".into()), ("method", "exit".into())]));
        }
        self.writer = None;
    }

    fn finished(&mut self) -> bool {
        if self.reaped {
            return true;
        }
        let done = matches!(self.child.try_wait(), Ok(Some(_)));
        if done {
            // whatever the server started may still be running
            sys::kill_group(self.child.id());
            self.reaped = true;
        }
        done
    }

    /// Stop the server and everything it started.
    fn kill(&mut self) {
        if !self.reaped {
            sys::kill_group(self.child.id());
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.reaped = true;
        }
        self.writer = None;
        self.state = SState::Dead("stopped".into());
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if !self.reaped {
            sys::kill_group(self.child.id());
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[derive(Default)]
pub struct LspManager {
    pub servers: Vec<Server>,
    /// extension -> why no server is running
    pub hints: HashMap<String, String>,
    pub comp_due: Option<(Instant, Option<char>)>,
    /// bumped for every completion request; stale answers are dropped
    pub comp_seq: u64,
    /// the last completion list was marked `isIncomplete`
    pub comp_incomplete: bool,
    /// when servers last died unexpectedly (to stop restarting one that keeps crashing)
    pub crashes: Vec<Instant>,
}

impl LspManager {
    pub fn new() -> LspManager {
        LspManager::default()
    }
}

// ---------------------------------------------------------------------------------------
// editor integration

fn markdown_to_lines(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for l in s.replace('\r', "").lines() {
        let t = l.trim_end();
        if t.trim_start().starts_with("```") {
            continue;
        }
        out.push(t.to_string());
    }
    while out.first().is_some_and(|l| l.is_empty()) {
        out.remove(0);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out
}

fn hover_text(contents: &Json) -> String {
    match contents {
        Json::Str(s) => s.clone(),
        Json::Arr(v) => v.iter().map(hover_text).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n"),
        Json::Obj(_) => contents.get("value").as_str().unwrap_or("").to_string(),
        _ => String::new(),
    }
}

impl Editor {
    fn resolve_server(&self, ext: &str) -> Result<String, String> {
        if let Some((_, o)) = self.opts.lsp_overrides.iter().rev().find(|(e, _)| e == ext) {
            return match o {
                Some(c) => {
                    let prog = c.split_whitespace().next().unwrap_or("");
                    if which(prog).is_some() {
                        Ok(c.clone())
                    } else {
                        Err(format!("lsp: {} (configured for .{}) not found in PATH", prog, ext))
                    }
                }
                None => Err(String::new()),
            };
        }
        let def = LANGS.iter().find(|l| l.exts.contains(&ext)).ok_or_else(String::new)?;
        for c in def.cmds {
            let prog = c.split_whitespace().next().unwrap_or("");
            if which(prog).is_some() {
                return Ok(c.to_string());
            }
        }
        Err(format!("lsp: no language server for .{} found (install {})", ext, def.cmds[0].split_whitespace().next().unwrap_or("")))
    }

    pub fn lsp_attach(&mut self, idx: usize) {
        if !self.opts.lsp || idx >= self.docs.len() {
            return;
        }
        let Some(path) = self.docs[idx].buf.path.clone() else { return };
        if self.docs[idx].lsp.client.is_some() {
            return;
        }
        let abs = std::fs::canonicalize(&path).unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(&path));
        let ext = abs.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
        if ext.is_empty() {
            return;
        }
        let cmdline = match self.resolve_server(&ext) {
            Ok(c) => c,
            Err(why) => {
                if !why.is_empty() && !self.lsp.hints.contains_key(&ext) {
                    // announce a missing server only where one is expected (or the user configured it);
                    // for markdown, json, yaml... the hint just waits in `:lsp`
                    let expected = matches!(ext.as_str(), "rs" | "py" | "js" | "jsx" | "ts" | "tsx" | "c" | "h" | "cc" | "cpp" | "hpp" | "go" | "zig" | "lua" | "java" | "rb");
                    if expected || why.contains("configured") {
                        self.set_msg(why.clone());
                    }
                    self.lsp.hints.insert(ext, why);
                }
                return;
            }
        };
        let root = find_root(&abs, &ext);
        let si = match self.lsp.servers.iter().position(|s| s.cmdline == cmdline && s.root == root && s.alive()) {
            Some(i) => i,
            None => match Server::start(&cmdline, &root) {
                Ok(s) => {
                    self.lsp.servers.push(s);
                    self.lsp.servers.len() - 1
                }
                Err(e) => {
                    self.set_err(format!("lsp: {}", e));
                    return;
                }
            },
        };
        let uri = path_to_uri(&abs);
        let d = &mut self.docs[idx];
        d.lsp.client = Some(si);
        d.lsp.uri = uri.clone();
        d.lsp.path = abs;
        d.lsp.opened = true;
        d.lsp.sent_version = d.buf.version;
        d.lsp.sent = d.buf.lines.clone();
        d.lsp.sent_final_newline = d.buf.final_newline;
        d.lsp.dirty_since = None;
        let item = obj(vec![
            ("uri", uri.into()),
            ("languageId", language_id(&ext).into()),
            ("version", int(d.buf.version as i64)),
            ("text", d.buf.text().into()),
        ]);
        self.lsp.servers[si].notify("textDocument/didOpen", obj(vec![("textDocument", item)]));
    }

    pub fn lsp_detach(&mut self, idx: usize) {
        if idx >= self.docs.len() {
            return;
        }
        if let Some(si) = self.docs[idx].lsp.client.take() {
            let uri = self.docs[idx].lsp.uri.clone();
            if let Some(s) = self.lsp.servers.get_mut(si) {
                s.notify("textDocument/didClose", obj(vec![("textDocument", obj(vec![("uri", uri.into())]))]));
            }
        }
        self.docs[idx].diags.clear();
        self.docs[idx].lsp.opened = false;
    }

    /// Send pending edits of document `idx` to its server.
    pub fn lsp_flush(&mut self, idx: usize) {
        let Some(si) = self.docs[idx].lsp.client else { return };
        let d = &mut self.docs[idx];
        d.lsp.dirty_since = None;
        if d.buf.version == d.lsp.sent_version {
            return;
        }
        let Some(server) = self.lsp.servers.get_mut(si) else { return };
        if !server.alive() {
            return;
        }
        let same_text = d.buf.lines == d.lsp.sent && d.buf.final_newline == d.lsp.sent_final_newline;
        // An incremental change names lines by number and always ends at "line N, column 0", which
        // only exists when the text the server holds *and* the new text both end with a newline. A
        // server such as nil does not clamp positions: a range past the end makes it drop the
        // whole document ("File is out of sync"). In every other case send the whole text.
        let changes = if same_text || server.sync_kind == 0 {
            None
        } else if server.sync_kind == 2
            && d.buf.final_newline
            && d.lsp.sent_final_newline
            && !trivial(&d.buf.lines)
            && !trivial(&d.lsp.sent)
        {
            Some(line_change(&d.lsp.sent, &d.buf.lines))
        } else {
            Some(obj(vec![("text", d.buf.text().into())]))
        };
        if let Some(c) = changes {
            let params = obj(vec![
                ("textDocument", obj(vec![("uri", d.lsp.uri.clone().into()), ("version", int(d.buf.version as i64))])),
                ("contentChanges", Json::Arr(vec![c])),
            ]);
            server.notify("textDocument/didChange", params);
        }
        d.lsp.sent_version = d.buf.version;
        d.lsp.sent = d.buf.lines.clone();
        d.lsp.sent_final_newline = d.buf.final_newline;
    }

    /// Close and re-open every document of server `si` with its current text. For when a server
    /// says it lost track of a file: after that it knows exactly what we have.
    fn lsp_resync(&mut self, si: usize) {
        let now = Instant::now();
        let Some(server) = self.lsp.servers.get_mut(si) else { return };
        if server.last_resync.is_some_and(|t| now.duration_since(t) < Duration::from_secs(2)) {
            return; // do not loop if it keeps failing
        }
        server.last_resync = Some(now);
        let mut any = false;
        for d in self.docs.iter_mut() {
            if d.lsp.client != Some(si) {
                continue;
            }
            any = true;
            let ext = d.lsp.path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
            server.notify("textDocument/didClose", obj(vec![("textDocument", obj(vec![("uri", d.lsp.uri.clone().into())]))]));
            let item = obj(vec![
                ("uri", d.lsp.uri.clone().into()),
                ("languageId", language_id(&ext).into()),
                ("version", int(d.buf.version as i64)),
                ("text", d.buf.text().into()),
            ]);
            server.notify("textDocument/didOpen", obj(vec![("textDocument", item)]));
            d.lsp.sent_version = d.buf.version;
            d.lsp.sent = d.buf.lines.clone();
            d.lsp.sent_final_newline = d.buf.final_newline;
            d.lsp.dirty_since = None;
        }
        if any {
            let name = self.lsp.servers[si].name.clone();
            self.set_msg(format!("lsp: {} lost track of a file; sent it again (retry the command)", name));
        }
    }

    pub fn lsp_saved(&mut self) {
        let idx = self.cur;
        self.lsp_flush(idx);
        let d = &self.docs[idx];
        if let Some(si) = d.lsp.client {
            let params = obj(vec![("textDocument", obj(vec![("uri", d.lsp.uri.clone().into())]))]);
            if let Some(s) = self.lsp.servers.get_mut(si) {
                s.notify("textDocument/didSave", params);
            }
        }
    }

    pub fn lsp_restart(&mut self) {
        self.lsp_shutdown();
        self.lsp.servers.clear();
        self.lsp.hints.clear();
        self.lsp.comp_due = None;
        self.completion = None;
        for d in self.docs.iter_mut() {
            d.lsp = DocLsp::default();
            d.diags.clear();
        }
        for i in 0..self.docs.len() {
            self.lsp_attach(i);
        }
        self.set_msg("language servers restarted");
    }

    /// Stop every server: ask politely, then wait a moment, then kill.
    pub fn lsp_shutdown(&mut self) {
        for s in self.lsp.servers.iter_mut() {
            s.begin_shutdown();
        }
        let end = Instant::now() + Duration::from_millis(250);
        loop {
            let mut all = true;
            for s in self.lsp.servers.iter_mut() {
                if !s.finished() {
                    all = false;
                }
            }
            if all || Instant::now() >= end {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        for s in self.lsp.servers.iter_mut() {
            if !s.finished() {
                s.kill();
            } else {
                s.state = SState::Dead("stopped".into());
            }
        }
    }

    /// How long the event loop may sleep (ms), so debounce timers fire on time.
    pub fn poll_timeout_ms(&self) -> i32 {
        let now = Instant::now();
        let mut best = 250i64;
        let mut consider = |due: Instant| {
            let ms = due.saturating_duration_since(now).as_millis() as i64;
            best = best.min(ms.max(1));
        };
        for d in &self.docs {
            if let Some(t) = d.lsp.dirty_since {
                consider(t + Duration::from_millis(120));
            }
        }
        if let Some((due, _)) = self.lsp.comp_due {
            consider(due);
        }
        best as i32
    }

    /// Housekeeping after every wake-up of the event loop.
    pub fn tick(&mut self) {
        self.tree_tick();
        self.lsp_process_messages();
        let now = Instant::now();
        for i in 0..self.docs.len() {
            if let Some(t) = self.docs[i].lsp.dirty_since {
                if now.duration_since(t) >= Duration::from_millis(120) {
                    self.lsp_flush(i);
                }
            }
        }
        if let Some((due, trig)) = self.lsp.comp_due {
            if now >= due {
                self.lsp.comp_due = None;
                if self.mode == Mode::Insert {
                    self.request_completion(trig);
                }
            }
        }
    }

    fn lsp_process_messages(&mut self) {
        let mut inbox: Vec<(usize, Msg)> = Vec::new();
        for (i, s) in self.lsp.servers.iter_mut().enumerate() {
            // (an empty or disconnected channel both just end the batch; a server that went away
            // has already queued its `Closed` notice)
            while let Ok(m) = s.rx.try_recv() {
                inbox.push((i, m));
            }
        }
        for (si, m) in inbox {
            match m {
                Msg::Closed => self.server_closed(si),
                Msg::Json(j) => self.handle_message(si, j),
            }
        }
    }

    fn server_closed(&mut self, si: usize) {
        let unexpected = match self.lsp.servers.get_mut(si) {
            Some(s) => {
                let was = s.alive();
                if was {
                    s.state = SState::Dead("exited".into());
                    s.writer = None;
                }
                was
            }
            None => false,
        };
        let mut orphans = Vec::new();
        for (i, d) in self.docs.iter_mut().enumerate() {
            if d.lsp.client == Some(si) {
                d.lsp.client = None;
                d.diags.clear();
                orphans.push(i);
            }
        }
        if !unexpected {
            // a server we had already given up on: say why, since nothing else will
            if let Some(SState::Dead(why)) = self.lsp.servers.get(si).map(|s| s.state.clone()) {
                if why == "not reading its input" {
                    let name = self.lsp.servers[si].name.clone();
                    self.set_err(format!("lsp: {} stopped: it was not reading its input (restart with :lsprestart)", name));
                }
            }
            return;
        }
        // reap the process (and whatever it started), then find out why it went
        if let Some(s) = self.lsp.servers.get_mut(si) {
            s.kill();
        }
        let name = self.lsp.servers[si].name.clone();
        let mut tail = self.lsp.servers[si].stderr_tail(1);
        if tail.is_empty() {
            // the stderr reader may still be a moment behind the process exit
            std::thread::sleep(Duration::from_millis(40));
            tail = self.lsp.servers[si].stderr_tail(1);
        }
        let why = tail.pop();
        // A server that dies while we are using it (nil on a bad request, rust-analyzer on a panic)
        // is started again and gets its documents back, unless it keeps dying.
        let now = Instant::now();
        self.lsp.crashes.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
        self.lsp.crashes.push(now);
        if orphans.is_empty() || self.lsp.crashes.len() > 3 {
            match why {
                Some(w) => self.set_err(format!("lsp: {} exited: {}", name, w)),
                None => self.set_err(format!("lsp: {} exited (see :lsp, restart with :lsprestart)", name)),
            }
            return;
        }
        for i in orphans {
            self.docs[i].lsp = DocLsp::default();
            self.lsp_attach(i);
        }
        let detail = why.map(|w| format!(": {}", w)).unwrap_or_default();
        self.set_msg(format!("lsp: {} exited{}; restarted", name, detail));
    }

    fn handle_message(&mut self, si: usize, j: Json) {
        let method = j.get("method").as_str().map(|s| s.to_string());
        let id = j.get("id").clone();
        match (method, id.is_null()) {
            (Some(m), false) => self.server_request(si, &m, &id, j.get("params")),
            (Some(m), true) => self.server_notification(si, &m, j.get("params")),
            (None, false) => {
                let key = id.as_i64().unwrap_or(-1);
                let Some(server) = self.lsp.servers.get_mut(si) else { return };
                let pend = server.pending.remove(&key);
                if let Some(p) = pend {
                    if !j.get("error").is_null() {
                        let code = j.get("error").get("code").as_i64().unwrap_or(0);
                        let msg = j.get("error").get("message").as_str().unwrap_or("error").to_string();
                        self.response_error(si, p, code, &msg);
                    } else {
                        self.handle_response(si, p, j.get("result"));
                    }
                }
            }
            _ => {}
        }
    }

    fn server_request(&mut self, si: usize, method: &str, id: &Json, params: &Json) {
        match method {
            "workspace/configuration" => {
                let n = params.get("items").arr().len();
                self.lsp.servers[si].respond(id, Json::Arr(vec![Json::Null; n]));
            }
            "client/registerCapability" | "client/unregisterCapability" | "window/workDoneProgress/create" | "window/showMessageRequest" => {
                self.lsp.servers[si].respond(id, Json::Null);
            }
            "workspace/workspaceFolders" => {
                let root = self.lsp.servers[si].root.clone();
                let f = obj(vec![("uri", path_to_uri(&root).into()), ("name", "root".into())]);
                self.lsp.servers[si].respond(id, Json::Arr(vec![f]));
            }
            "workspace/applyEdit" => {
                let res = self.apply_workspace_edit(params.get("edit"), None);
                let r = obj(vec![("applied", res.is_ok().into())]);
                self.lsp.servers[si].respond(id, r);
                if let Err(e) = res {
                    self.set_err(e);
                }
            }
            _ => self.lsp.servers[si].respond_error(id, -32601, "method not found"),
        }
    }

    fn server_notification(&mut self, si: usize, method: &str, params: &Json) {
        match method {
            "textDocument/publishDiagnostics" => self.on_diagnostics(params),
            "window/showMessage" => {
                if params.get("type").as_i64().unwrap_or(4) <= 2 {
                    let m = params.get("message").as_str().unwrap_or("").lines().next().unwrap_or("").to_string();
                    let name = self.lsp.servers[si].name.clone();
                    self.set_msg(format!("{}: {}", name, m));
                }
            }
            "$/progress" => {
                let token = match params.get("token") {
                    Json::Str(s) => s.clone(),
                    Json::Num(n) => n.to_string(),
                    _ => return,
                };
                let v = params.get("value");
                let s = &mut self.lsp.servers[si];
                match v.get("kind").as_str() {
                    Some("end") => {
                        s.progress.remove(&token);
                    }
                    Some(_) => {
                        let mut text = String::new();
                        if let Some(t) = v.get("title").as_str() {
                            text.push_str(t);
                        }
                        if let Some(m) = v.get("message").as_str() {
                            if !text.is_empty() {
                                text.push(' ');
                            }
                            text.push_str(m);
                        }
                        if let Some(p) = v.get("percentage").as_i64() {
                            text.push_str(&format!(" {}%", p));
                        }
                        if text.is_empty() {
                            text = s.progress.get(&token).cloned().unwrap_or_default();
                        }
                        if s.progress.len() < 64 || s.progress.contains_key(&token) {
                            s.progress.insert(token, text);
                        }
                    }
                    None => {}
                }
            }
            _ => {}
        }
    }

    fn doc_for_uri(&self, uri: &str) -> Option<usize> {
        if let Some(i) = self.docs.iter().position(|d| d.lsp.client.is_some() && d.lsp.uri == uri) {
            return Some(i);
        }
        let p = uri_to_path(uri)?;
        self.docs.iter().position(|d| d.lsp.client.is_some() && d.lsp.path == p)
    }

    fn on_diagnostics(&mut self, params: &Json) {
        let Some(idx) = self.doc_for_uri(params.get("uri").as_str().unwrap_or("")) else { return };
        let lines = &self.docs[idx].buf.lines;
        // (a server can send any number of them: keep what is useful and cheap to draw)
        let mut out: Vec<Diagnostic> = params
            .get("diagnostics")
            .arr()
            .iter()
            .take(5000)
            .map(|d| {
                let (start, end) = range_from_json(lines, d.get("range"));
                Diagnostic {
                    start,
                    end,
                    severity: d.get("severity").as_i64().unwrap_or(1).clamp(1, 4) as u8,
                    message: d.get("message").as_str().unwrap_or("").chars().take(4000).collect(),
                    source: d.get("source").as_str().unwrap_or("").chars().take(100).collect(),
                }
            })
            .collect();
        out.sort_by(|a, b| a.start.cmp(&b.start).then(a.severity.cmp(&b.severity)));
        self.docs[idx].diags = out;
    }

    // ---- responses -----------------------------------------------------------------

    fn response_error(&mut self, si: usize, p: Pending, code: i64, msg: &str) {
        // -32800 (cancelled) and -32801 (content modified) just mean "that answer is stale"; the
        // next keystroke asks again
        if code == -32800 || code == -32801 {
            return;
        }
        // nil answers "File not loaded" for a document it dropped after a failed change
        let lost = msg.contains("not loaded") || msg.contains("out of sync");
        match p {
            Pending::Initialize => {
                self.lsp.servers[si].state = SState::Dead(format!("initialize failed: {}", msg));
                self.set_err(format!("lsp: {}: initialize failed: {}", self.lsp.servers[si].name, msg));
            }
            Pending::Completion { .. } | Pending::Shutdown => {
                if lost {
                    self.lsp_resync(si);
                }
            }
            _ => {
                if lost {
                    self.lsp_resync(si);
                } else {
                    let first = msg.lines().next().unwrap_or("error");
                    self.set_err(format!("lsp: {}", first));
                }
            }
        }
    }

    fn handle_response(&mut self, si: usize, p: Pending, result: &Json) {
        match p {
            Pending::Initialize => self.lsp.servers[si].become_ready(result),
            Pending::Hover => {
                let lines = markdown_to_lines(&hover_text(result.get("contents")));
                if lines.is_empty() {
                    self.set_msg("No hover information");
                } else {
                    self.popup = Some(Popup { title: "hover".into(), lines, scroll: 0 });
                }
            }
            Pending::Definition => self.show_locations(result, "definition"),
            Pending::References => self.show_locations(result, "references"),
            Pending::Completion { uri, row, seq } => self.on_completion(&uri, row, seq, result),
            Pending::Rename { versions } => {
                if result.is_null() {
                    self.set_err("lsp: rename not possible here");
                } else if let Err(e) = self.apply_workspace_edit(result, Some(&versions)) {
                    self.set_err(e);
                }
            }
            Pending::Formatting { uri, version } => {
                let Some(idx) = self.docs.iter().position(|d| d.lsp.uri == uri) else { return };
                let edits = result.arr().to_vec();
                if edits.is_empty() {
                    self.set_msg("Already formatted");
                } else if self.docs[idx].buf.version != version {
                    // the edits describe the text as it was when we asked
                    self.set_err("lsp: the buffer changed while formatting; nothing applied (run it again)");
                } else {
                    self.apply_lsp_edits(idx, &edits);
                    self.clamp_cursor();
                    self.set_msg(format!("formatted ({} edits)", edits.len()));
                }
            }
            Pending::Shutdown => {}
        }
    }

    fn mark_doc_dirty(&mut self, idx: usize) {
        let d = &mut self.docs[idx];
        if d.lsp.client.is_some() && d.buf.version != d.lsp.sent_version && d.lsp.dirty_since.is_none() {
            d.lsp.dirty_since = Some(Instant::now());
        }
    }

    /// Column in `path`'s line `row` for an LSP UTF-16 offset, using the open buffer when there is one.
    fn char_col(&self, path: &Path, row: usize, u16col: usize) -> usize {
        for d in &self.docs {
            if let Some(p) = &d.buf.path {
                if p == path || std::fs::canonicalize(p).ok().as_deref() == Some(path) {
                    return d.buf.lines.get(row).map_or(0, |l| from_utf16_col(l, u16col));
                }
            }
        }
        match read_prefix(path, 8 << 20) {
            Ok(t) => t.lines().nth(row).map_or(0, |l| from_utf16_col(&l.chars().collect::<Vec<_>>(), u16col)),
            Err(_) => 0,
        }
    }

    fn show_locations(&mut self, result: &Json, what: &str) {
        let list: Vec<&Json> = match result {
            Json::Arr(v) => v.iter().collect(),
            Json::Obj(_) => vec![result],
            _ => Vec::new(),
        };
        let mut locs: Vec<(PathBuf, Pos)> = Vec::new();
        for l in list {
            // Location {uri, range} or LocationLink {targetUri, targetSelectionRange}
            let (uri, range) = if !l.get("targetUri").is_null() {
                (l.get("targetUri").as_str(), l.get("targetSelectionRange"))
            } else {
                (l.get("uri").as_str(), l.get("range"))
            };
            let Some(path) = uri.and_then(uri_to_path) else { continue };
            let row = range.get("start").get("line").as_usize().unwrap_or(0);
            let u = range.get("start").get("character").as_usize().unwrap_or(0);
            let col = self.char_col(&path, row, u);
            locs.push((path, (row, col)));
        }
        if locs.is_empty() {
            self.set_msg(format!("No {} found", what));
            return;
        }
        if locs.len() == 1 {
            let (p, pos) = locs.remove(0);
            if let Err(e) = self.goto_location(&p, pos) {
                self.set_err(e);
            }
            return;
        }
        let cwd = std::env::current_dir().unwrap_or_default();
        let mut cache: HashMap<PathBuf, Vec<String>> = HashMap::new();
        let mut items = Vec::new();
        for (p, pos) in locs.into_iter().take(500) {
            if !cache.contains_key(&p) {
                let open = self.docs.iter().find(|d| d.lsp.path == p || d.buf.path.as_deref() == Some(p.as_path()));
                let lines: Vec<String> = match open {
                    Some(d) => d.buf.lines.iter().map(|l| l.iter().collect()).collect(),
                    // only a look at the start of the file, whatever the server pointed at
                    None => read_prefix(&p, 2 << 20).map(|t| t.lines().map(|s| s.to_string()).collect()).unwrap_or_default(),
                };
                cache.insert(p.clone(), lines);
            }
            let text = cache[&p].get(pos.0).map(|s| s.trim().to_string()).unwrap_or_default();
            let rel = p.strip_prefix(&cwd).unwrap_or(&p).display().to_string();
            items.push(PickItem { label: format!("{}:{}:{}: {}", rel, pos.0 + 1, pos.1 + 1, text), target: PickTarget::Location { path: p, pos } });
        }
        self.picker = Some(Picker::new(what, items));
    }

    // ---- edits ---------------------------------------------------------------------------

    /// Apply LSP TextEdits (all positions refer to the current text) to document `idx`.
    pub fn apply_lsp_edits(&mut self, idx: usize, edits: &[Json]) -> usize {
        let lines = &self.docs[idx].buf.lines;
        let mut list: Vec<(Pos, Pos, String, usize)> = edits
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let text = e.get("newText").as_str()?.replace("\r\n", "\n");
                let (a, b) = range_from_json(lines, e.get("range"));
                Some((a, b, text, i))
            })
            .collect();
        // a server must not be able to make the buffer grow without bound
        let added: usize = list.iter().map(|e| e.2.len()).fold(0, |a, n| a.saturating_add(n));
        if !self.has_room(Some(added)) {
            return 0;
        }
        // from the end of the text to the start, so that earlier positions stay valid; inserts at one
        // spot end up in the order the server listed them
        list.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(b.3.cmp(&a.3)));
        // edits must not overlap (the spec forbids it): applying overlapping ones from the end would
        // mangle the text, so the later of two overlapping edits is dropped
        let mut kept: Vec<(Pos, Pos, String)> = Vec::with_capacity(list.len());
        for (a, b, t, _) in list {
            if let Some(prev) = kept.last() {
                if b > prev.0 {
                    continue;
                }
            }
            kept.push((a, b, t));
        }
        let n = kept.len();
        let c = self.docs[idx].cursor;
        self.docs[idx].buf.begin_group(c);
        for (s, e, t) in kept {
            self.docs[idx].buf.delete_range(s, e);
            self.docs[idx].buf.insert_str(s, &t);
        }
        let c = self.docs[idx].cursor;
        self.docs[idx].buf.end_group(c);
        let last = self.docs[idx].buf.line_count() - 1;
        let cur = &mut self.docs[idx].cursor;
        cur.0 = cur.0.min(last);
        self.mark_doc_dirty(idx);
        n
    }

    /// Apply a WorkspaceEdit. `versions` (for answers to requests we made) are the document versions
    /// at the time of the request: if any of the touched documents changed since, the edit's
    /// positions no longer fit and nothing is applied.
    pub fn apply_workspace_edit(&mut self, edit: &Json, versions: Option<&[(PathBuf, i32)]>) -> Result<(), String> {
        let mut per_file: Vec<(PathBuf, Vec<Json>)> = Vec::new();
        if let Json::Obj(changes) = edit.get("changes") {
            for (uri, edits) in changes {
                if let Some(p) = uri_to_path(uri) {
                    per_file.push((p, edits.arr().to_vec()));
                }
            }
        }
        for dc in edit.get("documentChanges").arr() {
            if dc.get("kind").as_str().is_some() {
                return Err("lsp: file create/rename/delete operations are not supported".into());
            }
            if let Some(p) = dc.get("textDocument").get("uri").as_str().and_then(uri_to_path) {
                per_file.push((p, dc.get("edits").arr().to_vec()));
            }
        }
        if per_file.is_empty() {
            return Err("lsp: nothing to change".into());
        }
        if let Some(vs) = versions {
            for (path, _) in &per_file {
                let Some(d) = self.docs.iter().find(|d| d.lsp.path == *path || d.buf.path.as_deref() == Some(path.as_path())) else { continue };
                let then = vs.iter().find(|(p, _)| *p == d.lsp.path).map(|(_, v)| *v);
                if then.is_some_and(|v| v != d.buf.version) {
                    return Err("lsp: a buffer changed while the server was working; nothing applied (try again)".into());
                }
            }
        }
        let mut total = 0;
        let nfiles = per_file.len();
        for (path, edits) in per_file {
            let idx = self.open_file(&path)?;
            total += self.apply_lsp_edits(idx, &edits);
        }
        self.clamp_cursor();
        self.set_msg(format!("{} change(s) in {} file(s){}", total, nfiles, if nfiles > 1 { " -- :wa to save" } else { "" }));
        Ok(())
    }

    // ---- user commands ------------------------------------------------------------------------

    fn client_for_cur(&mut self) -> Result<usize, String> {
        let idx = self.cur;
        let Some(si) = self.docs[idx].lsp.client else {
            let ext = self.buf().path.as_ref().and_then(|p| p.extension()).map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
            return Err(match self.lsp.hints.get(&ext) {
                Some(h) => h.clone(),
                None if !self.opts.lsp => "lsp is disabled (:set lsp)".into(),
                None => "no language server for this buffer".into(),
            });
        };
        let state = self.lsp.servers[si].state.clone();
        let name = self.lsp.servers[si].name.clone();
        match state {
            SState::Ready => {
                self.lsp_flush(idx);
                Ok(si)
            }
            SState::Starting => Err(format!("{} is still starting", name)),
            SState::Dead(why) => Err(format!("{} is not running ({})", name, why)),
        }
    }

    fn position_params(&self) -> Json {
        let d = self.doc();
        obj(vec![("textDocument", obj(vec![("uri", d.lsp.uri.clone().into())])), ("position", lsp_pos(&d.buf.lines, d.cursor))])
    }

    pub fn lsp_command(&mut self, name: &str) {
        if name == "]d" || name == "[d" {
            self.goto_diagnostic(name == "]d");
            return;
        }
        let si = match self.client_for_cur() {
            Ok(s) => s,
            Err(e) => {
                self.set_err(e);
                return;
            }
        };
        let (method, cap, pending, params) = match name {
            "K" => ("textDocument/hover", "hoverProvider", Pending::Hover, self.position_params()),
            "gd" => ("textDocument/definition", "definitionProvider", Pending::Definition, self.position_params()),
            "gr" => {
                let mut p = self.position_params();
                if let Json::Obj(v) = &mut p {
                    v.push(("context".into(), obj(vec![("includeDeclaration", true.into())])));
                }
                ("textDocument/references", "referencesProvider", Pending::References, p)
            }
            _ => return,
        };
        if !self.lsp.servers[si].supports(cap) {
            let n = self.lsp.servers[si].name.clone();
            self.set_err(format!("{} does not support this", n));
            return;
        }
        self.lsp.servers[si].request(method, params, pending);
    }

    pub fn lsp_rename(&mut self, new: &str) -> Result<(), String> {
        let si = self.client_for_cur()?;
        if !self.lsp.servers[si].supports("renameProvider") {
            return Err(format!("{} does not support rename", self.lsp.servers[si].name));
        }
        let mut p = self.position_params();
        if let Json::Obj(v) = &mut p {
            v.push(("newName".into(), new.into()));
        }
        let versions: Vec<(PathBuf, i32)> = self.docs.iter().filter(|d| d.lsp.client.is_some()).map(|d| (d.lsp.path.clone(), d.buf.version)).collect();
        self.lsp.servers[si].request("textDocument/rename", p, Pending::Rename { versions });
        self.set_msg("renaming...");
        Ok(())
    }

    pub fn lsp_format(&mut self) -> Result<(), String> {
        let si = self.client_for_cur()?;
        if !self.lsp.servers[si].supports("documentFormattingProvider") {
            return Err(format!("{} does not support formatting", self.lsp.servers[si].name));
        }
        let (tabs, w) = self.indent_style();
        let uri = self.doc().lsp.uri.clone();
        let version = self.buf().version;
        let p = obj(vec![
            ("textDocument", obj(vec![("uri", uri.clone().into())])),
            ("options", obj(vec![("tabSize", num(w)), ("insertSpaces", (!tabs).into())])),
        ]);
        self.lsp.servers[si].request("textDocument/formatting", p, Pending::Formatting { uri, version });
        Ok(())
    }

    // ---- diagnostics UI --------------------------------------------------------------------------------

    pub fn diag_counts(&self) -> (usize, usize) {
        let d = &self.doc().diags;
        (d.iter().filter(|x| x.severity == 1).count(), d.iter().filter(|x| x.severity == 2).count())
    }

    pub fn cursor_diag_message(&self) -> Option<(String, u8)> {
        let (r, c) = self.cursor();
        let d = self.doc();
        // the most severe one under the cursor, else the most severe on the line
        let on_pos = d.diags.iter().filter(|x| x.start <= (r, c) && (r, c) < x.end.max((x.start.0, x.start.1 + 1))).min_by_key(|x| x.severity);
        let hit = on_pos.or_else(|| d.diags.iter().filter(|x| x.start.0 == r).min_by_key(|x| x.severity))?;
        let mut m = hit.message.lines().next().unwrap_or("").to_string();
        if !hit.source.is_empty() {
            m = format!("[{}] {}", hit.source, m);
        }
        Some((m, hit.severity))
    }

    fn goto_diagnostic(&mut self, forward: bool) {
        let cur = self.cursor();
        let n = self.doc().diags.len();
        if n == 0 {
            self.set_msg("No diagnostics");
            return;
        }
        let d = &self.doc().diags;
        let i = if forward {
            d.iter().position(|x| x.start > cur).unwrap_or(0)
        } else {
            d.iter().rposition(|x| x.start < cur).unwrap_or(n - 1)
        };
        let t = d[i].clone();
        self.push_jump();
        self.move_to(t.start);
        self.set_msg(format!("[{}/{}] {}", i + 1, n, t.message.lines().next().unwrap_or("")));
        self.msg_err = t.severity == 1;
    }

    pub fn diag_picker(&mut self) {
        let Some(path) = self.buf().path.clone() else {
            self.set_err("diagnostics need a file name");
            return;
        };
        let d = &self.doc().diags;
        if d.is_empty() {
            self.set_msg("No diagnostics");
            return;
        }
        let items: Vec<PickItem> = d
            .iter()
            .map(|x| {
                let sev = match x.severity {
                    1 => "E",
                    2 => "W",
                    3 => "I",
                    _ => "H",
                };
                PickItem {
                    label: format!("{}:{} {} {}", x.start.0 + 1, x.start.1 + 1, sev, x.message.lines().next().unwrap_or("")),
                    target: PickTarget::Location { path: path.clone(), pos: x.start },
                }
            })
            .collect();
        self.picker = Some(Picker::new("diagnostics", items));
    }

    pub fn lsp_label(&self) -> Option<String> {
        let si = self.doc().lsp.client?;
        let s = self.lsp.servers.get(si)?;
        Some(match &s.state {
            SState::Starting => format!("{} (starting)", s.name),
            SState::Ready => match s.progress.values().next() {
                Some(p) => format!("{}: {}", s.name, p.chars().take(32).collect::<String>()),
                None => s.name.clone(),
            },
            SState::Dead(_) => format!("{} (stopped)", s.name),
        })
    }

    pub fn lsp_status(&self) -> String {
        let mut out = String::new();
        if !self.opts.lsp {
            out.push_str("language servers are disabled (:set lsp)\n\n");
        }
        if self.lsp.servers.is_empty() {
            out.push_str("no language servers running\n\n");
        }
        for (i, s) in self.lsp.servers.iter().enumerate() {
            let state = match &s.state {
                SState::Starting => "starting".to_string(),
                SState::Ready => "ready".to_string(),
                SState::Dead(w) => format!("stopped ({})", w),
            };
            let docs = self.docs.iter().filter(|d| d.lsp.client == Some(i)).count();
            out.push_str(&format!("{}  [{}]\n  command: {}\n  root:    {}\n  buffers: {}\n", s.name, state, s.cmdline, s.root.display(), docs));
            if !s.alive() {
                for l in s.stderr_tail(5) {
                    out.push_str(&format!("  stderr:  {}\n", l));
                }
            }
            if s.state == SState::Ready {
                let feats: Vec<&str> = [
                    ("completionProvider", "completion"),
                    ("hoverProvider", "hover"),
                    ("definitionProvider", "definition"),
                    ("referencesProvider", "references"),
                    ("renameProvider", "rename"),
                    ("documentFormattingProvider", "format"),
                ]
                .iter()
                .filter(|(k, _)| s.supports(k))
                .map(|(_, n)| *n)
                .collect();
                out.push_str(&format!("  features: {}\n", feats.join(", ")));
                if let Some(p) = s.progress.values().next() {
                    out.push_str(&format!("  busy:    {}\n", p));
                }
            }
            out.push('\n');
        }
        if !self.lsp.hints.is_empty() {
            out.push_str("not started:\n");
            let mut h: Vec<&String> = self.lsp.hints.values().collect();
            h.sort();
            for why in h {
                out.push_str(&format!("  {}\n", why));
            }
            out.push('\n');
        }
        out.push_str("configure with `lsp <ext> <command>` in ~/.config/ved/config\n");
        out
    }

    // ---- completion ------------------------------------------------------------------------------------------

    /// Characters that make up a name for completion. Nix names may contain `-`
    /// (`home-manager`, `pytest-cov`), which would otherwise close the popup at every dash.
    fn comp_word_char(&self, c: char) -> bool {
        is_word_char(c) || (c == '-' && self.buf().path.as_ref().and_then(|p| p.extension()).is_some_and(|e| e == "nix"))
    }

    fn word_start(&self) -> usize {
        let (r, c) = self.cursor();
        let line = &self.lines()[r];
        let mut s = c.min(line.len());
        while s > 0 && self.comp_word_char(line[s - 1]) {
            s -= 1;
        }
        s
    }

    /// True when the current buffer has a ready server that offers completion.
    fn can_complete(&self) -> bool {
        match self.doc().lsp.client.and_then(|si| self.lsp.servers.get(si)) {
            Some(s) => s.state == SState::Ready && s.supports("completionProvider"),
            None => false,
        }
    }

    /// Called after a character was typed (`Some(c)`) or deleted (`None`) in insert mode.
    pub fn after_typing(&mut self, c: Option<char>) {
        // while snippet placeholders are being filled in, Tab belongs to them, so no popup comes up on
        // its own (Ctrl-Space still opens one)
        if !self.opts.autocomplete || self.mode != Mode::Insert || self.snippet.is_some() || !self.can_complete() {
            return;
        }
        let si = self.doc().lsp.client.unwrap_or(0);
        match c {
            Some(ch) if self.comp_word_char(ch) => {
                if self.completion.is_some() {
                    self.refilter_completion();
                }
                let col = self.cursor().1;
                let len = col.saturating_sub(self.word_start());
                if len >= 2 && (self.completion.is_none() || self.lsp.comp_incomplete) {
                    self.lsp.comp_due = Some((Instant::now() + Duration::from_millis(70), None));
                }
            }
            Some(ch) => {
                self.completion = None;
                if self.lsp.servers[si].triggers.contains(&ch) {
                    self.lsp.comp_due = Some((Instant::now() + Duration::from_millis(40), Some(ch)));
                }
            }
            None => {
                if self.completion.is_some() {
                    self.refilter_completion();
                }
            }
        }
    }

    pub fn trigger_completion(&mut self, _manual: bool) {
        if self.can_complete() {
            self.request_completion(None);
        } else {
            self.buffer_word_completion();
        }
    }

    fn request_completion(&mut self, trigger: Option<char>) {
        if !self.can_complete() {
            return;
        }
        let Some(si) = self.doc().lsp.client else { return };
        let idx = self.cur;
        self.lsp_flush(idx);
        let mut params = self.position_params();
        let ctx = match trigger {
            Some(c) => obj(vec![("triggerKind", Json::Num(2.0)), ("triggerCharacter", c.to_string().into())]),
            None => obj(vec![("triggerKind", Json::Num(1.0))]),
        };
        if let Json::Obj(v) = &mut params {
            v.push(("context".into(), ctx));
        }
        let uri = self.doc().lsp.uri.clone();
        let row = self.cursor().0;
        self.lsp.comp_seq += 1;
        let seq = self.lsp.comp_seq;
        self.lsp.servers[si].request("textDocument/completion", params, Pending::Completion { uri, row, seq });
    }

    fn on_completion(&mut self, uri: &str, row: usize, seq: u64, result: &Json) {
        if seq != self.lsp.comp_seq || self.mode != Mode::Insert || self.doc().lsp.uri != uri || self.cursor().0 != row {
            return;
        }
        self.lsp.comp_incomplete = result.get("isIncomplete").as_bool().unwrap_or(false);
        let items_json: &[Json] = match result {
            Json::Arr(v) => v,
            Json::Obj(_) => result.get("items").arr(),
            _ => &[],
        };
        let line = self.lines()[row].clone();
        let start_col = self.word_start();
        let mut items: Vec<CompItem> = Vec::with_capacity(items_json.len().min(2000));
        for it in items_json.iter().take(2000) {
            let label: String = it.get("label").as_str().unwrap_or("").chars().take(500).collect();
            if label.is_empty() {
                continue;
            }
            let snippet = it.get("insertTextFormat").as_i64() == Some(2);
            let te = it.get("textEdit");
            let (raw, replace_from) = if !te.is_null() {
                let range = if !te.get("range").is_null() { te.get("range") } else { te.get("insert") };
                let rf = if range.get("start").get("line").as_usize() == Some(row) {
                    Some(from_utf16_col(&line, range.get("start").get("character").as_usize().unwrap_or(0)))
                } else {
                    None
                };
                (te.get("newText").as_str().unwrap_or(&label).chars().take(20_000).collect::<String>(), rf)
            } else {
                (it.get("insertText").as_str().unwrap_or(&label).chars().take(20_000).collect::<String>(), None)
            };
            let insert = if snippet { crate::snippet::parse_snippet(&raw).text } else { raw.clone() };
            items.push(CompItem {
                label: label.clone(),
                insert,
                detail: it.get("detail").as_str().or_else(|| it.get("labelDetails").get("detail").as_str()).unwrap_or("").lines().next().unwrap_or("").to_string(),
                kind: it.get("kind").as_i64().unwrap_or(0) as u8,
                filter: it.get("filterText").as_str().unwrap_or(&label).chars().take(500).collect(),
                sort: it.get("sortText").as_str().unwrap_or(&label).chars().take(500).collect(),
                replace_from,
                extra: it.get("additionalTextEdits").arr().iter().take(200).cloned().collect(),
                snippet: if snippet { Some(raw) } else { None },
            });
        }
        if items.is_empty() {
            self.completion = None;
            return;
        }
        let start = items.iter().find_map(|i| i.replace_from).map_or(start_col, |s| s.min(start_col));
        self.completion = Some(Completion { items, shown: Vec::new(), sel: 0, start_col: start });
        self.refilter_completion();
    }

    pub fn refilter_completion(&mut self) {
        let (r, col) = self.cursor();
        let Some(c) = self.completion.as_mut() else { return };
        let line = &self.docs[self.cur].buf.lines[r];
        if col < c.start_col || col > line.len() {
            self.completion = None;
            return;
        }
        let prefix: Vec<char> = line[c.start_col..col].iter().flat_map(|ch| ch.to_lowercase()).collect();
        let prefix_s: String = prefix.iter().collect();
        // (class, fuzzy score, index): prefix matches come first and keep the server's own order
        // (sortText); only the weaker, non-prefix matches are ranked by the fuzzy score.
        let mut scored: Vec<(u8, i32, usize)> = Vec::new();
        for (i, it) in c.items.iter().enumerate() {
            if prefix.is_empty() || it.filter.to_lowercase().starts_with(&prefix_s) {
                scored.push((2, 0, i));
            } else if let Some(s) = fuzzy_score(&prefix, &it.filter) {
                scored.push((1, s, i));
            }
        }
        let items = &c.items;
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then_with(|| items[a.2].sort.cmp(&items[b.2].sort)).then_with(|| items[a.2].label.cmp(&items[b.2].label))
        });
        c.shown = scored.into_iter().take(200).map(|(_, _, i)| i).collect();
        c.sel = 0;
        if c.shown.is_empty() {
            self.completion = None;
        }
    }

    pub fn completion_move(&mut self, d: i32) {
        if let Some(c) = self.completion.as_mut() {
            let n = c.shown.len() as i32;
            if n > 0 {
                c.sel = (c.sel as i32 + d).rem_euclid(n) as usize;
            }
        }
    }

    /// True when the highlighted option would insert exactly what is already typed.
    pub fn completion_is_noop(&self) -> bool {
        let Some(c) = &self.completion else { return false };
        let Some(&i) = c.shown.get(c.sel) else { return false };
        let it = &c.items[i];
        let (r, col) = self.cursor();
        let start = it.replace_from.unwrap_or(c.start_col).min(col);
        it.extra.is_empty() && self.lines()[r][start..col].iter().copied().eq(it.insert.chars())
    }

    /// Insert the highlighted option. Returns how many typed characters it replaced and the text that
    /// went in (so the caller can record it for dot-repeat).
    pub fn completion_accept(&mut self) -> Option<(usize, String)> {
        let c = self.completion.take()?;
        let &i = c.shown.get(c.sel)?;
        let it = c.items[i].clone();
        let (r, col) = self.cursor();
        let start = it.replace_from.unwrap_or(c.start_col).min(col);
        let idx = self.cur;
        let lines = self.docs[idx].buf.lines.clone();
        // a request still in flight (or about to be sent) was for the text just replaced
        self.lsp.comp_due = None;
        self.lsp.comp_seq += 1;
        // snippet items: insert their text, then walk through the placeholders (see snippet.rs)
        let snip = it.snippet.as_ref().map(|raw| {
            let indent: String = lines[r][..start].iter().take_while(|ch| **ch == ' ' || **ch == '\t').collect();
            let unit: String = self.indent_unit().into_iter().collect();
            crate::snippet::parse_snippet(&crate::snippet::adjust_indent(raw, &indent, &unit))
        });
        let text = snip.as_ref().map_or_else(|| it.insert.clone(), |s| s.text.clone());
        // header names come with their own closing `>` / `"`; on `#include <|>` that one is already
        // there, so it is replaced instead of doubled
        let mut end_col = col;
        if it.snippet.is_none() {
            if let Some(closer) = self.include_closer(r, start) {
                if text.ends_with(closer) && lines[r].get(col) == Some(&closer) {
                    end_col = col + 1;
                }
            }
        }
        // the main replacement plus additional edits (auto-imports), applied as one undo step
        let mut edits: Vec<(Pos, Pos, String)> = it
            .extra
            .iter()
            .filter_map(|e| {
                let (a, b) = range_from_json(&lines, e.get("range"));
                Some((a, b, e.get("newText").as_str()?.replace("\r\n", "\n")))
            })
            .collect();
        // rows the extra edits add above the cursor line (an edit that reaches the cursor line is
        // not counted: the main replacement is placed relative to it)
        let mut shift: isize = 0;
        for (s, e, t) in &edits {
            if e.0 < r {
                shift += t.matches('\n').count() as isize - (e.0 - s.0) as isize;
            }
        }
        edits.push(((r, start), (r, end_col), text.clone()));
        edits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        let c0 = self.cursor();
        self.docs[idx].buf.begin_group(c0);
        let mut end_cursor = (r, start);
        for (s, e, t) in edits {
            self.docs[idx].buf.delete_range(s, e);
            let end = self.docs[idx].buf.insert_str(s, &t);
            if s == (r, start) && e == (r, end_col) {
                end_cursor = end;
            }
        }
        let c1 = self.cursor();
        self.docs[idx].buf.end_group(c1);
        let row = (end_cursor.0 as isize + shift).max(0) as usize;
        let last = self.docs[idx].buf.line_count() - 1;
        self.docs[idx].cursor = (row.min(last), end_cursor.1);
        // a directory (`sys/`): list what is inside it right away
        if snip.is_none() && text.ends_with('/') {
            self.lsp.comp_due = Some((Instant::now() + Duration::from_millis(40), None));
        }
        if let Some(s) = &snip {
            // the text starts where the replaced word did (rows may have been added above by extra edits)
            let first = (r as isize + shift).max(0) as usize;
            self.snippet_begin(s, (first.min(last), start));
        }
        self.mark_doc_dirty(idx);
        Some((col - start, text))
    }

    /// Fallback completion from words already in the buffer.
    fn buffer_word_completion(&mut self) {
        let (r, col) = self.cursor();
        let start = self.word_start();
        let prefix: String = self.lines()[r][start..col.min(self.lines()[r].len())].iter().collect();
        if prefix.is_empty() {
            return;
        }
        let lp = prefix.to_lowercase();
        let mut seen = std::collections::HashSet::new();
        let mut items = Vec::new();
        for (row, line) in self.lines().iter().enumerate().take(50_000) {
            let mut i = 0;
            while i < line.len() {
                if is_word_char(line[i]) {
                    let s = i;
                    while i < line.len() && is_word_char(line[i]) {
                        i += 1;
                    }
                    let w: String = line[s..i].iter().collect();
                    let is_cursor_word = row == r && s <= col && col <= i;
                    if !is_cursor_word && w.chars().count() >= 3 && w.to_lowercase().starts_with(&lp) && w != prefix && seen.insert(w.clone()) {
                        items.push(CompItem { label: w.clone(), insert: w.clone(), detail: "buffer".into(), filter: w.clone(), sort: w, ..Default::default() });
                    }
                } else {
                    i += 1;
                }
            }
        }
        if items.is_empty() {
            self.set_msg("No completions");
            return;
        }
        items.sort_by(|a, b| a.sort.cmp(&b.sort));
        self.completion = Some(Completion { items, shown: Vec::new(), sel: 0, start_col: start });
        self.refilter_completion();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &[&str]) -> Vec<Vec<char>> {
        s.iter().map(|l| l.chars().collect()).collect()
    }

    #[test]
    fn uri_roundtrip() {
        let p = Path::new("/tmp/a b/\u{e9}.rs");
        let u = path_to_uri(p);
        assert_eq!(u, "file:///tmp/a%20b/%C3%A9.rs");
        assert_eq!(uri_to_path(&u).unwrap(), p);
        assert!(uri_to_path("http://x").is_none());
        // a stray '%' and a multi-byte character right after it must not panic
        assert_eq!(uri_to_path("file:///a%\u{e9}b").unwrap(), Path::new("/a%\u{e9}b"));
        assert_eq!(uri_to_path("file:///a%4").unwrap(), Path::new("/a%4"));
    }

    #[test]
    fn utf16_columns() {
        let l: Vec<char> = "a\u{1f600}b".chars().collect(); // the emoji is 2 UTF-16 units
        assert_eq!(to_utf16_col(&l, 2), 3);
        assert_eq!(from_utf16_col(&l, 3), 2);
        assert_eq!(from_utf16_col(&l, 1), 1);
        assert_eq!(from_utf16_col(&l, 99), 3);
    }

    #[test]
    fn markdown_hover_cleanup() {
        let l = markdown_to_lines("```rust\nfn main()\n```\n\nSome docs\n");
        assert_eq!(l, vec!["fn main()", "", "Some docs"]);
    }

    #[test]
    fn hover_shapes() {
        let j = Json::parse(r#"{"kind":"markdown","value":"x"}"#).unwrap();
        assert_eq!(hover_text(&j), "x");
        let j = Json::parse(r#"["a",{"language":"rust","value":"b"}]"#).unwrap();
        assert_eq!(hover_text(&j), "a\n\nb");
    }

    #[test]
    fn incremental_changes_cover_the_edit() {
        let c = line_change(&lines(&["a", "b", "c"]), &lines(&["a", "x", "y", "c"]));
        assert_eq!(c.to_string(), r#"{"range":{"start":{"line":1,"character":0},"end":{"line":2,"character":0}},"text":"x\ny\n"}"#);
        // append at the end of a document that ends with a newline
        let c = line_change(&lines(&["a"]), &lines(&["a", "b"]));
        assert_eq!(c.to_string(), r#"{"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":0}},"text":"b\n"}"#);
        // delete trailing lines
        let c = line_change(&lines(&["a", "b"]), &lines(&["a"]));
        assert_eq!(c.to_string(), r#"{"range":{"start":{"line":1,"character":0},"end":{"line":2,"character":0}},"text":""}"#);
        // edit inside one line
        let c = line_change(&lines(&["a", "b", "c"]), &lines(&["a", "bx", "c"]));
        assert_eq!(c.to_string(), r#"{"range":{"start":{"line":1,"character":0},"end":{"line":2,"character":0}},"text":"bx\n"}"#);
    }

    #[test]
    fn reader_parses_frames() {
        let b1 = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let b2 = "{\"a\":\"\u{e9}\u{1f600}\"}";
        let mut data = Vec::new();
        for b in [b1, b2] {
            data.extend(format!("Content-Length: {}\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n{}", b.len(), b).into_bytes());
        }
        let (tx, rx) = sync_channel(16);
        spawn_reader(std::io::Cursor::new(data), tx);
        let t = Duration::from_secs(5);
        match rx.recv_timeout(t).unwrap() {
            Msg::Json(j) => assert_eq!(j.get("id").as_i64(), Some(1)),
            _ => panic!("expected a message"),
        }
        match rx.recv_timeout(t).unwrap() {
            Msg::Json(j) => assert_eq!(j.get("a").as_str(), Some("\u{e9}\u{1f600}")),
            _ => panic!("expected a message"),
        }
        assert!(matches!(rx.recv_timeout(t).unwrap(), Msg::Closed));
    }

    #[test]
    fn frames_with_absurd_sizes_or_headers_end_the_connection_instead_of_allocating() {
        // a length of many gigabytes
        let mut r = std::io::Cursor::new(b"Content-Length: 99999999999\r\n\r\nxx".to_vec());
        assert!(matches!(read_frame(&mut r), Frame::End));
        // a header line that never ends
        let mut long = vec![b'a'; MAX_HEADER_LINE * 4];
        long.push(b'\n');
        assert!(matches!(read_frame(&mut std::io::Cursor::new(long)), Frame::End));
        // a body that is shorter than announced
        let mut r = std::io::Cursor::new(b"Content-Length: 10\r\n\r\nxx".to_vec());
        assert!(matches!(read_frame(&mut r), Frame::End));
        // header names are case-insensitive, and a block without a length is skipped, not fatal
        let mut r = std::io::Cursor::new(b"content-length: 2\r\n\r\n{}Content-Type: x\r\n\r\n".to_vec());
        assert!(matches!(read_frame(&mut r), Frame::Body(b) if b == b"{}"));
        assert!(matches!(read_frame(&mut r), Frame::Skip));
        // invalid UTF-8 in a body is replaced, not a reason to drop the message
        let (tx, rx) = sync_channel(4);
        let body: &[u8] = b"{\"a\":\"x\xffy\"}";
        let mut data = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        data.extend_from_slice(body);
        spawn_reader(std::io::Cursor::new(data), tx);
        match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            Msg::Json(j) => assert_eq!(j.get("a").as_str(), Some("x\u{fffd}y")),
            _ => panic!("expected a message"),
        }
    }

    #[test]
    fn programs_are_never_looked_up_in_the_current_directory() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("ved-which-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let evil = base.join("zz-fake-server");
        std::fs::write(&evil, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&evil, std::fs::Permissions::from_mode(0o755)).unwrap();
        let abs = std::env::join_paths([base.as_path()]).unwrap();
        assert_eq!(which_in("zz-fake-server", Some(abs), None), Some(evil.clone()));
        for hostile in ["", ".", "./", "relative/dir", ":/nonexistent", "/nonexistent:"] {
            let found = which_in("zz-fake-server", Some(hostile.into()), Some("not-absolute".into()));
            assert_eq!(found, None, "PATH={:?}", hostile);
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn server_ranges_are_ordered_and_inside_the_buffer() {
        let l = lines(&["abc", "de"]);
        let r = Json::parse(r#"{"start":{"line":1,"character":1},"end":{"line":0,"character":2}}"#).unwrap();
        assert_eq!(range_from_json(&l, &r), ((0, 2), (1, 1)));
        let r = Json::parse(r#"{"start":{"line":99,"character":99},"end":{"line":100,"character":0}}"#).unwrap();
        assert_eq!(range_from_json(&l, &r), ((1, 2), (1, 2)));
        let r = Json::parse(r#"{"start":{"line":-1,"character":1e300},"end":{}}"#).unwrap();
        let (a, b) = range_from_json(&l, &r);
        assert!(a <= b && a.0 < 2 && a.1 <= 3);
    }

    #[test]
    fn root_detection_prefers_workspace() {
        let base = std::env::temp_dir().join(format!("ved-root-{}", std::process::id()));
        let sub = base.join("crates/a/src");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(base.join("Cargo.toml"), "[workspace]\nmembers=[\"crates/a\"]\n").unwrap();
        std::fs::write(base.join("crates/a/Cargo.toml"), "[package]\nname=\"a\"\n").unwrap();
        assert_eq!(find_root(&sub.join("lib.rs"), "rs"), base);
        std::fs::write(base.join("package.json"), "{}").unwrap();
        assert_eq!(find_root(&base.join("x.js"), "js"), base);
        let _ = std::fs::remove_dir_all(&base);
    }
}
