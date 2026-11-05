//! Editor state, insert mode, command line, search and registers.
//! Normal/visual mode lives in `normal.rs`, ex commands in `ex.rs`, drawing in `render.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::buffer::{Buffer, Pos};
use crate::config::Options;
use crate::input::{Event, Key};
use crate::lsp::{CompItem, Diagnostic, DocLsp, LspManager};
use crate::motion::{self, class};
use crate::screen::char_width;
use crate::term::TermPane;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Normal,
    Insert,
    Visual,
    VisualLine,
    Command,
    Search,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Editor,
    TermInsert,
    TermNormal,
}

#[derive(Clone, Debug)]
pub struct Reg {
    pub text: Vec<char>,
    pub linewise: bool,
}

#[derive(Clone, Debug)]
pub struct Search {
    pub pat: Vec<char>,
    pub forward: bool,
    pub whole: bool,
}

#[derive(Default)]
pub struct InsertState {
    pub count: usize,
    /// `Some(below)` when insert was started by `o` / `O`.
    pub open_line: Option<bool>,
    pub keys: Vec<Key>,
    pub repeating: bool,
    pub pending_reg: bool,
    /// Row whose only content is auto-inserted indentation (cleared again if nothing is typed).
    pub ai_row: Option<usize>,
}

pub struct Completion {
    pub items: Vec<CompItem>,
    /// indices into `items` that match the current prefix, best first
    pub shown: Vec<usize>,
    pub sel: usize,
    /// column where the replaced word starts
    pub start_col: usize,
}

pub struct Doc {
    pub buf: Buffer,
    pub cursor: Pos,
    /// desired display column for vertical motions
    pub want: usize,
    pub top: usize,
    pub left: usize,
    pub marks: HashMap<char, Pos>,
    pub lsp: DocLsp,
    pub diags: Vec<Diagnostic>,
    pub hl: crate::highlight::HlCache,
}

impl Doc {
    pub fn new(buf: Buffer) -> Doc {
        Doc {
            buf,
            cursor: (0, 0),
            want: 0,
            top: 0,
            left: 0,
            marks: HashMap::new(),
            lsp: DocLsp::default(),
            diags: Vec::new(),
            hl: crate::highlight::HlCache::default(),
        }
    }

    pub fn name(&self) -> String {
        match &self.buf.path {
            Some(p) => p.to_string_lossy().to_string(),
            None => "[No Name]".to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum PickTarget {
    Location { path: PathBuf, pos: Pos },
    Buffer(usize),
}

#[derive(Clone, Debug)]
pub struct PickItem {
    pub label: String,
    pub target: PickTarget,
}

pub struct Picker {
    pub title: String,
    pub all: Vec<PickItem>,
    pub items: Vec<PickItem>,
    pub sel: usize,
    pub query: String,
}

/// Subsequence match; higher is better. None when `q` is not a subsequence of `s`.
pub fn fuzzy_score(q: &[char], s: &str) -> Option<i32> {
    if q.is_empty() {
        return Some(0);
    }
    let sc: Vec<char> = s.chars().flat_map(|c| c.to_lowercase()).collect();
    let mut qi = 0;
    let mut score = 0i32;
    let mut prev_match: Option<usize> = None;
    for (i, c) in sc.iter().enumerate() {
        if qi < q.len() && *c == q[qi] {
            score += 10;
            if prev_match == Some(i.wrapping_sub(1)) {
                score += 15; // consecutive
            }
            if i == 0 || matches!(sc[i - 1], '/' | '_' | '-' | '.' | ' ') {
                score += 12; // start of a path segment / word
            }
            prev_match = Some(i);
            qi += 1;
        }
    }
    if qi < q.len() {
        return None;
    }
    // prefer matches in the file name over the directory part, and short paths
    let name_start = sc.iter().rposition(|c| *c == '/').map_or(0, |i| i + 1);
    if let Some(pos) = prev_match {
        if pos >= name_start {
            score += 8;
        }
    }
    Some(score * 4 - sc.len() as i32)
}

impl Picker {
    pub fn new(title: &str, items: Vec<PickItem>) -> Picker {
        let mut p = Picker { title: title.to_string(), all: items, items: Vec::new(), sel: 0, query: String::new() };
        p.refilter();
        p
    }

    pub fn refilter(&mut self) {
        let q: Vec<char> = self.query.chars().flat_map(|c| c.to_lowercase()).collect();
        if q.is_empty() {
            self.items = self.all.clone();
        } else {
            let mut scored: Vec<(i32, usize)> =
                self.all.iter().enumerate().filter_map(|(i, it)| fuzzy_score(&q, &it.label).map(|s| (s, i))).collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.items = scored.into_iter().take(500).map(|(_, i)| self.all[i].clone()).collect();
        }
        self.sel = 0;
    }
}

pub struct Popup {
    pub title: String,
    pub lines: Vec<String>,
    pub scroll: usize,
}

pub struct Editor {
    pub opts: Options,
    pub docs: Vec<Doc>,
    pub cur: usize,
    pub alt: Option<usize>,
    pub mode: Mode,
    pub prev_mode: Mode,
    pub focus: Focus,
    pub rows: usize,
    pub cols: usize,
    /// text area size from the last layout
    pub view_h: usize,
    pub view_w: usize,
    pub keybuf: Vec<Key>,
    pub regs: HashMap<char, Reg>,
    pub macros: HashMap<char, Vec<Key>>,
    pub recording: Option<char>,
    pub macro_buf: Vec<Key>,
    pub replay_depth: usize,
    /// When the key being handled must be done by (see `timed_out`).
    deadline: Option<std::time::Instant>,
    interrupted: bool,
    pub in_dot: bool,
    pub cur_change: Vec<Key>,
    pub last_change: Vec<Key>,
    pub insert: InsertState,
    pub visual_anchor: Pos,
    pub last_visual: Option<(Mode, Pos, Pos)>,
    pub last_find: Option<(char, bool, bool)>,
    pub last_search: Option<Search>,
    pub hl_search: bool,
    pub cmdline: String,
    pub cmd_kind: char,
    pub cmd_hist: Vec<String>,
    pub search_hist: Vec<String>,
    pub hist_idx: Option<usize>,
    pub msg: String,
    pub msg_err: bool,
    pub quit: bool,
    pub jumps: Vec<(usize, Pos)>,
    pub jump_idx: usize,
    pub picker: Option<Picker>,
    pub popup: Option<Popup>,
    pub completion: Option<Completion>,
    /// Active snippet placeholders (Tab / Shift-Tab jump between them).
    pub snippet: Option<crate::snippet::Session>,
    /// Escape sequences (OSC 52 clipboard) waiting to be written to the host terminal.
    pub out_seq: String,
    pub term: Option<TermPane>,
    /// The pane is tucked away (Alt-w): the shell keeps running, nothing of it is drawn.
    pub term_hidden: bool,
    /// The project panel (file tree); whether it is shown is `opts.sidebar`.
    pub tree: crate::tree::Tree,
    /// The panel's right edge is being dragged with the mouse (button held down).
    pub sidebar_drag: bool,
    /// A question in the command-line row (a name for a new file, delete: yes or no).
    pub prompt: Option<crate::panelops::Prompt>,
    /// A tab with unsaved changes had its x clicked once: a second click discards the changes.
    pub confirm_close: Option<usize>,
    pub lsp: LspManager,
    pub last_ex: Option<String>,
    pub ctrl_w_pending: bool,
    /// Set by commands that must not become the dot-repeat target (u, ctrl-r, ., @).
    pub no_dot: bool,
    pub last_sub: Option<(String, String, String)>,
    /// Set by `open_line`; becomes `insert.ai_row` once insert mode starts.
    pub pending_ai: Option<usize>,
    /// `d/pat` style operator waiting for the search to be typed: (op, register, count).
    pub search_op: Option<(crate::normal::Op, Option<char>, Option<usize>)>,
}

// ---- display-column helpers --------------------------------------------

pub fn cell_width(c: char, col: usize, ts: usize) -> usize {
    if c == '\t' {
        ts - col % ts
    } else if (c as u32) < 0x20 || c as u32 == 0x7f {
        2
    } else {
        char_width(c)
    }
}

pub fn display_col(line: &[char], idx: usize, ts: usize) -> usize {
    let mut col = 0;
    for &c in line.iter().take(idx) {
        col += cell_width(c, col, ts);
    }
    col
}

/// Index of the char covering display column `target` (line.len() when past the end).
pub fn idx_at_display(line: &[char], target: usize, ts: usize) -> usize {
    let mut col = 0;
    for (i, &c) in line.iter().enumerate() {
        let w = cell_width(c, col, ts);
        if col + w > target {
            return i;
        }
        col += w;
    }
    line.len()
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len() * 4 / 3 + 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn matches_at(line: &[char], i: usize, pat: &[char], icase: bool) -> bool {
    if i + pat.len() > line.len() {
        return false;
    }
    for (k, p) in pat.iter().enumerate() {
        let c = line[i + k];
        if c == *p {
            continue;
        }
        if icase && c.to_lowercase().eq(p.to_lowercase()) {
            continue;
        }
        return false;
    }
    true
}

fn whole_ok(line: &[char], i: usize, plen: usize) -> bool {
    let before = i == 0 || !motion::is_word_char(line[i - 1]);
    let after = i + plen >= line.len() || !motion::is_word_char(line[i + plen]);
    before && after
}

/// First match at column >= start.
pub fn find_in_line_fwd(line: &[char], pat: &[char], start: usize, icase: bool, whole: bool) -> Option<usize> {
    if pat.is_empty() || line.len() < pat.len() {
        return None;
    }
    (start..=line.len() - pat.len()).find(|&i| matches_at(line, i, pat, icase) && (!whole || whole_ok(line, i, pat.len())))
}

/// Last match at column < limit.
pub fn find_in_line_bwd(line: &[char], pat: &[char], limit: usize, icase: bool, whole: bool) -> Option<usize> {
    if pat.is_empty() || line.len() < pat.len() {
        return None;
    }
    let top = (line.len() - pat.len()).min(limit.saturating_sub(1));
    if limit == 0 {
        return None;
    }
    (0..=top).rev().find(|&i| matches_at(line, i, pat, icase) && (!whole || whole_ok(line, i, pat.len())))
}

impl Editor {
    pub fn new(opts: Options, rows: usize, cols: usize) -> Editor {
        Editor {
            opts,
            docs: vec![Doc::new(Buffer::new())],
            cur: 0,
            alt: None,
            mode: Mode::Normal,
            prev_mode: Mode::Normal,
            focus: Focus::Editor,
            rows,
            cols,
            view_h: rows.saturating_sub(2).max(1),
            view_w: cols,
            keybuf: Vec::new(),
            regs: HashMap::new(),
            macros: HashMap::new(),
            recording: None,
            macro_buf: Vec::new(),
            replay_depth: 0,
            deadline: None,
            interrupted: false,
            in_dot: false,
            cur_change: Vec::new(),
            last_change: Vec::new(),
            insert: InsertState::default(),
            visual_anchor: (0, 0),
            last_visual: None,
            last_find: None,
            last_search: None,
            hl_search: false,
            cmdline: String::new(),
            cmd_kind: ':',
            cmd_hist: Vec::new(),
            search_hist: Vec::new(),
            hist_idx: None,
            msg: String::new(),
            msg_err: false,
            quit: false,
            jumps: Vec::new(),
            jump_idx: 0,
            picker: None,
            popup: None,
            completion: None,
            snippet: None,
            out_seq: String::new(),
            term: None,
            term_hidden: false,
            tree: crate::tree::Tree::new(),
            sidebar_drag: false,
            prompt: None,
            confirm_close: None,
            lsp: LspManager::new(),
            last_ex: None,
            ctrl_w_pending: false,
            no_dot: false,
            last_sub: None,
            pending_ai: None,
            search_op: None,
        }
    }

    // ---- accessors ------------------------------------------------------

    pub fn doc(&self) -> &Doc {
        &self.docs[self.cur]
    }
    pub fn doc_mut(&mut self) -> &mut Doc {
        &mut self.docs[self.cur]
    }
    pub fn buf(&self) -> &Buffer {
        &self.docs[self.cur].buf
    }
    pub fn buf_mut(&mut self) -> &mut Buffer {
        &mut self.docs[self.cur].buf
    }
    pub fn cursor(&self) -> Pos {
        self.docs[self.cur].cursor
    }
    pub fn lines(&self) -> &[Vec<char>] {
        &self.docs[self.cur].buf.lines
    }
    pub fn line_len(&self, r: usize) -> usize {
        self.docs[self.cur].buf.lines[r].len()
    }

    pub fn set_msg(&mut self, s: impl Into<String>) {
        self.msg = s.into();
        self.msg_err = false;
    }
    pub fn set_err(&mut self, s: impl Into<String>) {
        self.msg = s.into();
        self.msg_err = true;
    }

    /// Keep the cursor inside the buffer for the current mode.
    pub fn clamp_cursor(&mut self) {
        let insert = self.mode == Mode::Insert;
        let d = &mut self.docs[self.cur];
        let last = d.buf.lines.len() - 1;
        if d.cursor.0 > last {
            d.cursor.0 = last;
        }
        let len = d.buf.lines[d.cursor.0].len();
        let max = if insert { len } else { len.saturating_sub(1) };
        if d.cursor.1 > max {
            d.cursor.1 = max;
        }
    }

    pub fn update_want(&mut self) {
        let ts = self.opts.tabstop;
        let d = &mut self.docs[self.cur];
        d.want = display_col(&d.buf.lines[d.cursor.0], d.cursor.1, ts);
    }

    /// Move the cursor, clamp it and remember the column for vertical motion.
    pub fn move_to(&mut self, p: Pos) {
        self.docs[self.cur].cursor = p;
        self.clamp_cursor();
        self.update_want();
    }

    pub fn first_nonblank(&self, r: usize) -> usize {
        let l = &self.lines()[r];
        let n = l.iter().take_while(|c| **c == ' ' || **c == '\t').count();
        if n >= l.len() {
            l.len().saturating_sub(1)
        } else {
            n
        }
    }

    // ---- indentation ----------------------------------------------------

    /// (use_tabs, width) for the current buffer: always the configured style (2 spaces by default),
    /// except Makefiles, which need real tabs, and files when `detectindent` is on.
    /// With tabs one indent level is one tab, which is `tabstop` columns wide.
    pub fn indent_style(&self) -> (bool, usize) {
        let ts = self.opts.tabstop.max(1);
        if self.is_makefile() {
            return (true, ts);
        }
        if self.opts.detectindent {
            if let Some(i) = self.buf().indent {
                return (i.use_tabs, if i.use_tabs { ts } else { i.width });
            }
        }
        if self.opts.expandtab {
            (false, self.opts.shiftwidth)
        } else {
            (true, ts)
        }
    }

    fn is_makefile(&self) -> bool {
        match self.buf().path.as_ref() {
            Some(p) => {
                matches!(p.file_name().and_then(|n| n.to_str()), Some("Makefile") | Some("makefile") | Some("GNUmakefile"))
                    || p.extension().and_then(|e| e.to_str()) == Some("mk")
            }
            None => false,
        }
    }

    fn is_rust(&self) -> bool {
        self.buf().path.as_ref().and_then(|p| p.extension()).and_then(|e| e.to_str()) == Some("rs")
    }

    pub fn make_indent(&self, width: usize) -> Vec<char> {
        let (tabs, _) = self.indent_style();
        if tabs {
            let ts = self.opts.tabstop.max(1);
            let mut v = vec!['\t'; width / ts];
            v.extend(std::iter::repeat_n(' ', width % ts));
            v
        } else {
            vec![' '; width]
        }
    }

    pub fn indent_unit(&self) -> Vec<char> {
        let (tabs, w) = self.indent_style();
        if tabs {
            vec!['\t']
        } else {
            vec![' '; w]
        }
    }

    pub fn indent_width(&self, r: usize) -> usize {
        let l = &self.lines()[r];
        let n = l.iter().take_while(|c| **c == ' ' || **c == '\t').count();
        display_col(l, n, self.opts.tabstop)
    }

    /// Shift one line by `levels` (positive = right).
    pub fn shift_line(&mut self, r: usize, levels: isize) {
        let (_, sw) = self.indent_style();
        let line = self.lines()[r].clone();
        if line.is_empty() && levels > 0 {
            return;
        }
        let n = line.iter().take_while(|c| **c == ' ' || **c == '\t').count();
        let w = display_col(&line, n, self.opts.tabstop) as isize;
        let nw = (w + levels * sw as isize).max(0) as usize;
        let mut nl = self.make_indent(nw);
        nl.extend_from_slice(&line[n..]);
        self.buf_mut().set_line(r, nl);
    }

    // ---- files / buffers -----------------------------------------------

    fn pristine(d: &Doc) -> bool {
        d.buf.path.is_none() && !d.buf.modified && d.buf.lines.len() == 1 && d.buf.lines[0].is_empty()
    }

    pub fn open_file(&mut self, path: &Path) -> Result<usize, String> {
        let canon = std::fs::canonicalize(path).ok();
        for (i, d) in self.docs.iter().enumerate() {
            if let Some(p) = &d.buf.path {
                if p == path || (canon.is_some() && std::fs::canonicalize(p).ok() == canon) {
                    return Ok(i);
                }
            }
        }
        let buf = Buffer::open(path).map_err(|e| format!("{}: {}", path.display(), e))?;
        let doc = Doc::new(buf);
        let idx = if self.docs.len() == 1 && Self::pristine(&self.docs[0]) {
            self.docs[0] = doc;
            0
        } else {
            self.docs.push(doc);
            self.docs.len() - 1
        };
        self.lsp_attach(idx);
        Ok(idx)
    }

    pub fn switch_to(&mut self, idx: usize) {
        if idx >= self.docs.len() || idx == self.cur {
            return;
        }
        self.alt = Some(self.cur);
        self.cur = idx;
        self.snippet = None;
        self.mode = Mode::Normal;
        self.clamp_cursor();
        self.update_want();
    }

    pub fn goto_location(&mut self, path: &Path, pos: Pos) -> Result<(), String> {
        let idx = self.open_file(path)?;
        self.push_jump();
        self.switch_to(idx);
        self.move_to(pos);
        let h = self.view_h;
        self.doc_mut().top = pos.0.saturating_sub(h / 2);
        Ok(())
    }

    pub fn push_jump(&mut self) {
        let here = (self.cur, self.cursor());
        self.doc_mut().marks.insert('\'', here.1);
        self.jumps.truncate(self.jump_idx);
        if self.jumps.last() != Some(&here) {
            self.jumps.push(here);
        }
        self.jump_idx = self.jumps.len();
        if self.jumps.len() > 100 {
            self.jumps.remove(0);
            self.jump_idx = self.jumps.len();
        }
    }

    pub fn jump_older(&mut self) {
        if self.jump_idx == 0 {
            return;
        }
        if self.jump_idx == self.jumps.len() {
            let here = (self.cur, self.cursor());
            if self.jumps.last() != Some(&here) {
                self.jumps.push(here);
            }
            self.jump_idx = self.jumps.len() - 1;
        }
        if self.jump_idx == 0 {
            return;
        }
        self.jump_idx -= 1;
        let (d, p) = self.jumps[self.jump_idx];
        if d < self.docs.len() {
            self.switch_to(d);
            self.move_to(p);
        }
    }

    pub fn jump_newer(&mut self) {
        if self.jump_idx + 1 >= self.jumps.len() {
            return;
        }
        self.jump_idx += 1;
        let (d, p) = self.jumps[self.jump_idx];
        if d < self.docs.len() {
            self.switch_to(d);
            self.move_to(p);
        }
    }

    // ---- registers & clipboard -----------------------------------------

    pub fn copy_clipboard(&mut self, text: &str) {
        if text.len() <= 74_000 {
            self.out_seq.push_str(&format!("\x1b]52;c;{}\x07", base64(text.as_bytes())));
        }
    }

    fn read_clipboard(&self) -> Option<String> {
        let cmds: [(&str, &[&str]); 4] = [
            ("wl-paste", &["-n"]),
            ("xclip", &["-o", "-selection", "clipboard"]),
            ("xsel", &["-ob"]),
            ("pbpaste", &[]),
        ];
        for (c, a) in cmds {
            // only a program found in an absolute PATH entry (never one sitting in the project), and
            // one that does not answer within a couple of seconds is given up on (xclip can hang)
            let Some(path) = crate::lsp::which(c) else { continue };
            let mut cmd = std::process::Command::new(path);
            cmd.args(a);
            if let Ok(o) = crate::ex::run_command_limited(cmd, std::time::Duration::from_secs(2), 8 << 20) {
                if o.status.success() {
                    return Some(String::from_utf8_lossy(&o.stdout).to_string());
                }
            }
        }
        None
    }

    pub fn get_reg(&self, name: Option<char>) -> Option<Reg> {
        match name {
            None | Some('"') => self.regs.get(&'"').cloned(),
            Some('_') => None,
            Some('+') | Some('*') => match self.read_clipboard() {
                Some(s) => {
                    let linewise = s.ends_with('\n');
                    let t = s.strip_suffix('\n').unwrap_or(&s);
                    Some(Reg { text: t.chars().collect(), linewise })
                }
                None => self.regs.get(&'"').cloned(),
            },
            Some('/') => self.last_search.as_ref().map(|s| Reg { text: s.pat.clone(), linewise: false }),
            Some(':') => self.last_ex.as_ref().map(|s| Reg { text: s.chars().collect(), linewise: false }),
            Some('%') => self.buf().path.as_ref().map(|p| Reg { text: p.to_string_lossy().chars().collect(), linewise: false }),
            Some(c) => self.regs.get(&c.to_ascii_lowercase()).cloned(),
        }
    }

    /// Store text from a yank (`is_yank`) or a delete/change into the right registers.
    pub fn set_reg(&mut self, name: Option<char>, text: Vec<char>, linewise: bool, is_yank: bool) {
        if name == Some('_') {
            return;
        }
        let reg = Reg { text: text.clone(), linewise };
        match name {
            Some(c) if c.is_ascii_uppercase() => {
                let key = c.to_ascii_lowercase();
                let merged = match self.regs.get(&key) {
                    Some(old) => {
                        let mut t = old.text.clone();
                        if linewise || old.linewise {
                            t.push('\n');
                        }
                        t.extend(text.iter());
                        Reg { text: t, linewise: linewise || old.linewise }
                    }
                    None => reg.clone(),
                };
                self.regs.insert(key, merged.clone());
                self.regs.insert('"', merged);
            }
            Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {
                self.regs.insert(c, reg.clone());
                self.regs.insert('"', reg);
            }
            Some('+') | Some('*') => {
                self.regs.insert('"', reg);
                let mut s: String = text.iter().collect();
                if linewise {
                    s.push('\n');
                }
                self.copy_clipboard(&s);
            }
            _ => {
                self.regs.insert('"', reg.clone());
                if is_yank {
                    self.regs.insert('0', reg);
                    if self.opts.osc52 {
                        let mut s: String = text.iter().collect();
                        if linewise {
                            s.push('\n');
                        }
                        self.copy_clipboard(&s);
                    }
                } else if linewise || text.contains(&'\n') {
                    for i in (1..9).rev() {
                        let from = char::from(b'0' + i);
                        let to = char::from(b'0' + i + 1);
                        if let Some(r) = self.regs.get(&from).cloned() {
                            self.regs.insert(to, r);
                        }
                    }
                    self.regs.insert('1', reg);
                } else {
                    self.regs.insert('-', reg);
                }
            }
        }
    }

    // ---- main key entry -------------------------------------------------

    pub fn handle_event(&mut self, ev: Event) {
        // a key press ends a drag whose button release never arrived, and withdraws a "click again"
        if !matches!(ev.key, Key::Mouse(_)) {
            self.sidebar_drag = false;
            self.confirm_close = None;
        }
        // The terminal keys work from any mode, also while the terminal has focus, but not while a
        // command line or picker is taking input:
        //   Alt-s  move focus between editor and terminal (opening the pane first if need be)
        //   Alt-q  open the terminal (start a shell, show a hidden pane) and focus it
        //   Alt-w  hide the pane (the shell keeps running); again: show it
        //   Alt-e  close the pane and stop the shell
        let typing = matches!(self.mode, Mode::Command | Mode::Search) || self.picker.is_some() || self.prompt.is_some();
        if !typing {
            match ev.key {
                Key::Alt('s') => {
                    self.toggle_terminal();
                    return;
                }
                Key::Alt('q') => {
                    self.open_terminal();
                    return;
                }
                Key::Alt('w') => {
                    self.hide_terminal();
                    return;
                }
                Key::Alt('e') => {
                    self.close_terminal_key();
                    return;
                }
                // Alt-b is the project panel's key, but only where the editor has the keyboard: in
                // the terminal it is the shell's "back one word"
                Key::Alt('b') if self.focus == Focus::Editor => {
                    self.toggle_sidebar();
                    return;
                }
                _ => {}
            }
        }
        // the mouse is the same wherever the keyboard is; it never reaches macros or the shell as keys
        if let Key::Mouse(m) = ev.key {
            self.handle_mouse(m);
            return;
        }
        // a question in the command-line row has every key
        if self.prompt.is_some() {
            self.prompt_key(ev.key);
            return;
        }
        if self.focus != Focus::Editor {
            self.term_key(ev);
            return;
        }
        // the project panel has the keyboard (unless a command line, picker or popup is on top of it)
        if self.tree.focused && self.sidebar_cols() > 0 && !typing && self.popup.is_none() {
            self.tree_key(ev.key);
            return;
        }
        if self.tree.focused && self.sidebar_cols() == 0 {
            self.tree.focused = false;
        }
        self.handle_key(ev.key);
    }

    // ---- surviving internal errors ----------------------------------------------------------

    /// Write every modified buffer to the recovery directory (never over the original files).
    /// Returns what was written: (buffer name, recovered file) and what failed.
    pub fn emergency_save(&self) -> (Vec<(String, std::path::PathBuf)>, Vec<String>) {
        let mut saved = Vec::new();
        let mut failed = Vec::new();
        for (i, d) in self.docs.iter().enumerate() {
            if !d.buf.modified {
                continue;
            }
            let name = d.buf.path.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|| format!("unnamed-buffer-{}", i + 1));
            let mut text = d.buf.text();
            if d.buf.crlf {
                text = text.replace('\n', "\r\n");
            }
            match crate::recovery::save(&name, text.as_bytes()) {
                Ok(p) => saved.push((name, p)),
                Err(e) => failed.push(format!("{}: {}", name, e)),
            }
        }
        (saved, failed)
    }

    /// Put the editor back into a consistent state after an internal error interrupted something
    /// half way: at least one document, valid indices, a cursor inside the text, no leftover modes.
    pub fn repair(&mut self) {
        self.prompt = None;
        if self.docs.is_empty() {
            self.docs.push(Doc::new(Buffer::new()));
        }
        if self.cur >= self.docs.len() {
            self.cur = self.docs.len() - 1;
        }
        if self.alt.is_some_and(|a| a >= self.docs.len()) {
            self.alt = None;
        }
        for d in self.docs.iter_mut() {
            d.buf.abort_group();
            if d.buf.lines.is_empty() {
                d.buf.lines.push(Vec::new());
            }
            let last = d.buf.lines.len() - 1;
            d.cursor.0 = d.cursor.0.min(last);
            d.cursor.1 = d.cursor.1.min(d.buf.lines[d.cursor.0].len());
            d.top = d.top.min(last);
        }
        self.mode = Mode::Normal;
        self.prev_mode = Mode::Normal;
        self.keybuf.clear();
        self.completion = None;
        self.snippet = None;
        self.picker = None;
        self.popup = None;
        self.search_op = None;
        self.cmdline.clear();
        self.recording = None;
        self.replay_depth = 0;
        self.in_dot = false;
        self.deadline = None;
        self.interrupted = false;
        self.insert = InsertState::default();
        self.clamp_cursor();
    }

    /// How long one key press may keep the editor busy (a `100000@a` macro, `:g` with `:normal`...).
    const KEY_TIME_LIMIT: std::time::Duration = std::time::Duration::from_secs(5);

    /// True once the key being handled has run past its time limit. Loops that repeat other
    /// commands (counts, macros, `:normal`, `:g`) check it so that nothing can freeze the editor;
    /// the first time it trips it says so in the message line.
    pub fn timed_out(&mut self) -> bool {
        if self.interrupted {
            return true;
        }
        if let Some(d) = self.deadline {
            if std::time::Instant::now() > d {
                self.interrupted = true;
                self.set_err("Interrupted: that took too long (u undoes what it did)");
                return true;
            }
        }
        false
    }

    /// Is there room to add `extra` characters (`n` copies of something) to the buffer? Says so in
    /// the message line when there is not.
    pub fn has_room(&mut self, extra: Option<usize>) -> bool {
        let used = self.buf().char_count();
        match extra {
            Some(e) if used.saturating_add(e) <= crate::buffer::max_chars() => true,
            _ => {
                self.set_err(format!("That would make the buffer larger than {} Mi characters", crate::buffer::max_chars() >> 20));
                false
            }
        }
    }

    pub fn handle_key(&mut self, k: Key) {
        // the outermost key press sets the clock for everything it triggers
        let outer = self.deadline.is_none();
        if outer {
            self.deadline = Some(std::time::Instant::now() + Self::KEY_TIME_LIMIT);
            self.interrupted = false;
        }
        self.handle_key_inner(k);
        if outer {
            self.deadline = None;
        }
    }

    fn handle_key_inner(&mut self, k: Key) {
        if self.replay_depth == 0 && self.recording.is_some() {
            self.macro_buf.push(k.clone());
        }
        if self.mode != Mode::Command && self.mode != Mode::Search {
            self.msg.clear();
        }
        if self.picker.is_some() {
            self.picker_key(k);
            return;
        }
        if let Some(p) = self.popup.as_mut() {
            match k {
                Key::Esc | Key::Char('q') => {
                    self.popup = None;
                    return;
                }
                Key::Char('j') | Key::Down | Key::Ctrl('e') => {
                    p.scroll += 1;
                    return;
                }
                Key::Char('k') | Key::Up | Key::Ctrl('y') => {
                    p.scroll = p.scroll.saturating_sub(1);
                    return;
                }
                Key::Ctrl('d') | Key::PageDown | Key::Char(' ') => {
                    p.scroll += 10;
                    return;
                }
                Key::Ctrl('u') | Key::PageUp => {
                    p.scroll = p.scroll.saturating_sub(10);
                    return;
                }
                _ => {
                    self.popup = None;
                }
            }
        }
        match self.mode {
            Mode::Normal | Mode::Visual | Mode::VisualLine => self.normal_key(k),
            Mode::Insert => {
                // snippet placeholders: Tab / Shift-Tab jump, typing replaces the selected one
                if !self.snippet_key(&k) {
                    let snap = self.snippet_snap();
                    self.insert_key(k);
                    self.snippet_track(snap);
                }
            }
            Mode::Command | Mode::Search => self.cmdline_key(k),
        }
        self.mark_dirty();
    }

    /// Replay keys as if typed (dot-repeat and macros).
    pub fn replay_keys(&mut self, keys: Vec<Key>, dot: bool) {
        // replays can trigger replays (a macro that runs itself, `.` repeating `.`): bound the nesting
        // so that the stack cannot run out
        if self.replay_depth >= 32 {
            self.set_err("Replay nested too deeply");
            return;
        }
        self.replay_depth += 1;
        let was = self.in_dot;
        self.in_dot = self.in_dot || dot;
        for k in keys {
            if self.timed_out() {
                break;
            }
            self.handle_key(k);
        }
        self.in_dot = was;
        self.replay_depth -= 1;
    }

    fn picker_key(&mut self, k: Key) {
        let n = self.picker.as_ref().map_or(0, |p| p.items.len());
        match k {
            Key::Esc | Key::Ctrl('c') => self.picker = None,
            Key::Down | Key::Ctrl('n') | Key::Ctrl('j') => {
                if let Some(p) = self.picker.as_mut() {
                    if p.sel + 1 < n {
                        p.sel += 1;
                    }
                }
            }
            Key::Up | Key::Ctrl('p') | Key::Ctrl('k') => {
                if let Some(p) = self.picker.as_mut() {
                    p.sel = p.sel.saturating_sub(1);
                }
            }
            Key::PageDown => {
                if let Some(p) = self.picker.as_mut() {
                    p.sel = (p.sel + 10).min(n.saturating_sub(1));
                }
            }
            Key::PageUp => {
                if let Some(p) = self.picker.as_mut() {
                    p.sel = p.sel.saturating_sub(10);
                }
            }
            Key::Backspace | Key::Ctrl('h') => {
                if let Some(p) = self.picker.as_mut() {
                    p.query.pop();
                    p.refilter();
                }
            }
            Key::Ctrl('u') => {
                if let Some(p) = self.picker.as_mut() {
                    p.query.clear();
                    p.refilter();
                }
            }
            Key::Char(c) => {
                if let Some(p) = self.picker.as_mut() {
                    p.query.push(c);
                    p.refilter();
                }
            }
            Key::Enter => {
                if let Some(p) = self.picker.take() {
                    if let Some(item) = p.items.get(p.sel) {
                        match &item.target {
                            PickTarget::Location { path, pos } => {
                                if let Err(e) = self.goto_location(path, *pos) {
                                    self.set_err(e);
                                }
                            }
                            PickTarget::Buffer(i) => {
                                let i = *i;
                                self.switch_to(i);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Flag the current buffer for a (debounced) didChange if it changed.
    pub fn mark_dirty(&mut self) {
        let d = &mut self.docs[self.cur];
        if d.lsp.client.is_some() && d.buf.version != d.lsp.sent_version && d.lsp.dirty_since.is_none() {
            d.lsp.dirty_since = Some(Instant::now());
        }
    }

    /// Close the undo group opened by a normal-mode command.
    pub fn finish_group(&mut self) {
        let c = self.cursor();
        self.buf_mut().end_group(c);
    }

    // ---- insert mode ----------------------------------------------------

    pub fn enter_insert(&mut self, count: usize, open_line: Option<bool>) {
        self.mode = Mode::Insert;
        self.snippet = None;
        self.insert = InsertState { count: count.max(1), open_line, keys: Vec::new(), repeating: false, pending_reg: false, ai_row: None };
        self.insert.ai_row = self.pending_ai.take();
        self.completion = None;
    }

    pub fn open_line(&mut self, below: bool) {
        let r = self.cursor().0;
        let indent: Vec<char> = if self.opts.autoindent {
            let l = &self.lines()[r];
            let n = l.iter().take_while(|c| **c == ' ' || **c == '\t').count();
            let mut ind = l[..n].to_vec();
            // deepen after an opening bracket
            let last = l.iter().rev().find(|c| !c.is_whitespace()).copied();
            if below && matches!(last, Some('{') | Some('(') | Some('[')) {
                ind.extend(self.indent_unit());
            }
            ind
        } else {
            Vec::new()
        };
        let at = if below { r + 1 } else { r };
        let ilen = indent.len();
        self.buf_mut().insert_lines(at, vec![indent]);
        self.docs[self.cur].cursor = (at, ilen);
        self.pending_ai = if ilen > 0 { Some(at) } else { None };
    }

    pub fn insert_key(&mut self, k: Key) {
        // Completion popup: Tab / Shift-Tab (also Ctrl-n / Ctrl-p and the arrows) move down / up, Enter or
        // Ctrl-y accept, Ctrl-e dismisses. These keys are not recorded for dot-repeat; an accepted
        // completion is recorded as the text it inserted (see `record_completion`).
        if self.completion.is_some() && !self.insert.pending_reg {
            match k {
                Key::Tab | Key::Down | Key::Ctrl('n') => {
                    self.completion_move(1);
                    return;
                }
                Key::BackTab | Key::Up | Key::Ctrl('p') => {
                    self.completion_move(-1);
                    return;
                }
                Key::Ctrl('y') => {
                    if let Some((del, text)) = self.completion_accept() {
                        self.record_completion(del, &text);
                    }
                    return;
                }
                Key::Enter if !self.completion_is_noop() => {
                    if let Some((del, text)) = self.completion_accept() {
                        self.record_completion(del, &text);
                    }
                    return;
                }
                // the word is already complete: Enter is just a newline
                Key::Enter => self.completion = None,
                Key::Ctrl('e') => {
                    self.completion = None;
                    return;
                }
                _ => {}
            }
        }
        // while snippet placeholders are being filled in, the accepted snippet's plain text is what
        // dot-repeat replays: the keys typed over the placeholders are not recorded
        let in_snippet = self.snippet.is_some();
        if !self.in_dot && !in_snippet {
            self.cur_change.push(k.clone());
        }
        if !self.insert.repeating && k != Key::Esc && !in_snippet {
            self.insert.keys.push(k.clone());
        }
        // the row `{<CR>` just opened, if nothing has been typed on it yet
        let fresh_row = self.insert.ai_row;
        if !matches!(k, Key::Esc | Key::Ctrl('c') | Key::Enter) {
            self.insert.ai_row = None;
        }
        if self.insert.pending_reg {
            self.insert.pending_reg = false;
            if let Key::Char(c) = k {
                if let Some(r) = self.get_reg(Some(c)) {
                    let mut t = r.text.clone();
                    if r.linewise {
                        t.push('\n');
                    }
                    self.insert_text_at_cursor(&t);
                }
            }
            return;
        }
        match k {
            Key::Esc | Key::Ctrl('c') => self.leave_insert(),
            Key::Char(c) => {
                if !(self.opts.autopairs && self.overtype_block_closer(c, fresh_row)) {
                    self.insert_char(c);
                }
                self.after_typing(Some(c));
            }
            Key::Enter => {
                self.completion = None;
                self.insert_newline();
            }
            Key::Tab => self.insert_tab(),
            Key::BackTab | Key::Ctrl('d') => {
                let r = self.cursor().0;
                let before = self.line_len(r);
                self.shift_line(r, -1);
                let after = self.line_len(r);
                let c = self.cursor().1;
                self.docs[self.cur].cursor.1 = (c + after).saturating_sub(before);
            }
            Key::Ctrl('t') => {
                let r = self.cursor().0;
                let before = self.line_len(r);
                self.shift_line(r, 1);
                let after = self.line_len(r);
                let c = self.cursor().1;
                self.docs[self.cur].cursor.1 = (c + after).saturating_sub(before);
            }
            Key::Backspace | Key::Ctrl('h') => {
                self.insert_backspace();
                self.after_typing(None);
            }
            Key::Delete => {
                let (r, c) = self.cursor();
                if c < self.line_len(r) {
                    self.buf_mut().delete_range((r, c), (r, c + 1));
                } else if r + 1 < self.buf().line_count() {
                    self.buf_mut().delete_range((r, c), (r + 1, 0));
                }
            }
            Key::Ctrl('w') => {
                self.insert_delete_word();
                self.after_typing(None);
            }
            Key::Ctrl('u') => {
                let (r, c) = self.cursor();
                let fnb = self.lines()[r].iter().take_while(|ch| ch.is_whitespace()).count().min(c);
                let to = if c > fnb { fnb } else { 0 };
                self.buf_mut().delete_range((r, to), (r, c));
                self.docs[self.cur].cursor.1 = to;
            }
            Key::Ctrl('r') => self.insert.pending_reg = true,
            Key::Ctrl('n') | Key::Ctrl('p') | Key::Ctrl(' ') | Key::Ctrl('x') => self.trigger_completion(true),
            Key::Left => {
                let c = self.cursor().1;
                if c > 0 {
                    self.docs[self.cur].cursor.1 = c - 1;
                }
                self.completion = None;
            }
            Key::Right => {
                let (r, c) = self.cursor();
                if c < self.line_len(r) {
                    self.docs[self.cur].cursor.1 = c + 1;
                }
                self.completion = None;
            }
            Key::Up | Key::Down => {
                let (r, _) = self.cursor();
                let nr = if k == Key::Up { r.saturating_sub(1) } else { (r + 1).min(self.buf().line_count() - 1) };
                let ts = self.opts.tabstop;
                let want = self.doc().want;
                let col = idx_at_display(&self.lines()[nr].clone(), want, ts);
                self.docs[self.cur].cursor = (nr, col.min(self.line_len(nr)));
                self.completion = None;
            }
            Key::Home => self.docs[self.cur].cursor.1 = 0,
            Key::End => {
                let r = self.cursor().0;
                self.docs[self.cur].cursor.1 = self.line_len(r);
            }
            Key::Paste(s) => {
                let t: Vec<char> = s.replace("\r\n", "\n").replace('\r', "\n").chars().collect();
                self.insert_text_at_cursor(&t);
            }
            _ => {}
        }
        if self.mode == Mode::Insert {
            self.update_want();
        }
    }

    pub fn insert_text_at_cursor(&mut self, text: &[char]) {
        let at = self.cursor();
        let end = self.buf_mut().insert_text(at, text);
        self.docs[self.cur].cursor = end;
    }

    fn insert_char(&mut self, c: char) {
        if self.opts.autopairs && self.autopair(c) {
            return;
        }
        let (r, col) = self.cursor();
        // typing a closing bracket at the start of an indented line dedents it
        if matches!(c, '}' | ')' | ']') && self.opts.autoindent && col > 0 {
            let line = self.lines()[r].clone();
            if line[..col].iter().all(|ch| ch.is_whitespace()) && line[col..].iter().all(|ch| ch.is_whitespace()) {
                let (_, sw) = self.indent_style();
                let sw = sw.max(1);
                let w = display_col(&line, col, self.opts.tabstop);
                let nw = if w.is_multiple_of(sw) { w.saturating_sub(sw) } else { w / sw * sw };
                let mut nl = self.make_indent(nw);
                let nlen = nl.len();
                nl.push(c);
                self.buf_mut().set_line(r, nl);
                self.docs[self.cur].cursor = (r, nlen + 1);
                return;
            }
        }
        let end = self.buf_mut().insert_text((r, col), &[c]);
        self.docs[self.cur].cursor = end;
    }

    fn insert_newline(&mut self) {
        let (r, c) = self.cursor();
        let line = self.lines()[r].clone();
        let mut at = (r, c);
        if self.opts.autoindent && c > 0 && line.iter().all(|ch| ch.is_whitespace()) {
            // an auto-indented but otherwise empty line is cleaned when we leave it
            self.buf_mut().delete_range((r, 0), (r, line.len()));
            at = (r, 0);
        }
        let mut text: Vec<char> = vec!['\n'];
        let mut col = 0;
        let mut split_closer = false;
        if self.opts.autoindent {
            let ws = line.iter().take_while(|ch| **ch == ' ' || **ch == '\t').count();
            let indent: Vec<char> = line[..ws.min(c)].to_vec();
            let last_before = line[..c].iter().rev().find(|ch| !ch.is_whitespace()).copied();
            let first_after = line[c..].iter().find(|ch| !ch.is_whitespace()).copied();
            let opener = matches!(last_before, Some('{') | Some('(') | Some('['));
            let closer = matches!(
                (last_before, first_after),
                (Some('{'), Some('}')) | (Some('('), Some(')')) | (Some('['), Some(']'))
            );
            let colon = last_before == Some(':') && self.is_python_like();
            text.extend(indent.iter());
            col = indent.len();
            if opener || colon {
                let unit = self.indent_unit();
                text.extend(unit.iter());
                col += unit.len();
                if closer {
                    // `{|}` + Enter puts the closing bracket on its own line
                    text.push('\n');
                    text.extend(indent.iter());
                    split_closer = true;
                }
            }
        }
        self.buf_mut().insert_text(at, &text);
        self.docs[self.cur].cursor = (at.0 + 1, col);
        let rest_blank = line[c..].iter().all(|ch| ch.is_whitespace());
        self.insert.ai_row = if col > 0 && (rest_blank || split_closer) { Some(at.0 + 1) } else { None };
    }

    fn is_python_like(&self) -> bool {
        matches!(
            self.buf().path.as_ref().and_then(|p| p.extension()).and_then(|e| e.to_str()),
            Some("py" | "nim" | "yaml" | "yml")
        )
    }

    /// Tab always inserts one full indent: 2 spaces (or a real tab for Makefiles / `noexpandtab`).
    fn insert_tab(&mut self) {
        let (tabs, sw) = self.indent_style();
        if tabs {
            self.insert_text_at_cursor(&['\t']);
        } else {
            self.insert_text_at_cursor(&vec![' '; sw.max(1)]);
        }
        self.completion = None;
    }

    /// Make an accepted completion replayable by `.`, `3i` and macros: delete what was typed, retype the result.
    fn record_completion(&mut self, delete: usize, text: &str) {
        let mut keys: Vec<Key> = vec![Key::Backspace; delete];
        for ch in text.chars() {
            keys.push(if ch == '\n' { Key::Enter } else { Key::Char(ch) });
        }
        if !self.in_dot {
            self.cur_change.extend(keys.iter().cloned());
        }
        if !self.insert.repeating {
            self.insert.keys.extend(keys.iter().cloned());
        }
        if self.replay_depth == 0 && self.recording.is_some() {
            // macros record raw keys before they get here: drop the accept key and the Tab / arrow
            // presses that led to it, which would not find a popup when the macro is played back
            self.macro_buf.pop();
            while matches!(
                self.macro_buf.last(),
                Some(Key::Tab | Key::BackTab | Key::Down | Key::Up | Key::Ctrl('n') | Key::Ctrl('p'))
            ) {
                self.macro_buf.pop();
            }
            self.macro_buf.extend(keys);
        }
    }

    /// `{<CR>` leaves the closing bracket on the line below. Typing it on the still-empty indented line
    /// takes that one instead of adding a second: the empty line goes away and the cursor lands after it.
    fn overtype_block_closer(&mut self, c: char, fresh_row: Option<usize>) -> bool {
        if !matches!(c, ')' | ']' | '}') {
            return false;
        }
        let (r, _) = self.cursor();
        if fresh_row != Some(r) || r + 1 >= self.buf().line_count() {
            return false;
        }
        let below = &self.lines()[r + 1];
        let ws = below.iter().take_while(|ch| ch.is_whitespace()).count();
        if below.get(ws) != Some(&c) || !self.lines()[r].iter().all(|ch| ch.is_whitespace()) {
            return false;
        }
        self.buf_mut().delete_lines(r, r);
        self.docs[self.cur].cursor = (r, ws + 1);
        true
    }

    /// Auto-close brackets and quotes. Returns true when it handled the key.
    ///
    /// - typing a closer (or quote) right before the same character steps over it
    /// - an opener inserts the pair, but only when what follows is blank or a closer
    /// - quotes are left alone after a word character (`don't`) except string prefixes (`f"`, `r'`, `b"`),
    ///   after a backslash, and after `&` / `<` in Rust (lifetimes)
    /// - in Python files `"""` becomes `"""|"""`
    ///
    /// On a `#include <...` / `#include "...` line, with `upto` inside the file name: the delimiter that
    /// closes it (`>` or `"`), unless that is already typed before `upto`.
    pub fn include_closer(&self, r: usize, upto: usize) -> Option<char> {
        let line = self.lines().get(r)?;
        let line = &line[..upto.min(line.len())];
        let mut i = 0;
        let skip_ws = |i: &mut usize| {
            while *i < line.len() && (line[*i] == ' ' || line[*i] == '\t') {
                *i += 1;
            }
        };
        skip_ws(&mut i);
        if line.get(i) != Some(&'#') {
            return None;
        }
        i += 1;
        skip_ws(&mut i);
        let w = i;
        while i < line.len() && (line[i].is_ascii_alphabetic() || line[i] == '_') {
            i += 1;
        }
        let word: String = line[w..i].iter().collect();
        if !matches!(word.as_str(), "include" | "include_next" | "import") {
            return None;
        }
        skip_ws(&mut i);
        let closer = match line.get(i)? {
            '<' => '>',
            '"' => '"',
            _ => return None,
        };
        if line[i + 1..].contains(&closer) {
            return None;
        }
        Some(closer)
    }

    fn autopair(&mut self, c: char) -> bool {
        let (r, col) = self.cursor();
        let line = &self.docs[self.cur].buf.lines[r];
        let next = line.get(col).copied();
        let prev = col.checked_sub(1).map(|i| line[i]);
        let close = match c {
            '(' => ')',
            '[' => ']',
            '{' => '}',
            '"' | '\'' | '`' => c,
            ')' | ']' | '}' => {
                if next == Some(c) {
                    self.docs[self.cur].cursor.1 = col + 1;
                    return true;
                }
                return false;
            }
            // `#include <|>` (the closing bracket is already there): typing it steps over it
            '>' => {
                if next == Some('>') && self.include_closer(r, col) == Some('>') {
                    self.docs[self.cur].cursor.1 = col + 1;
                    return true;
                }
                return false;
            }
            _ => return false,
        };
        let is_quote = c == close;
        if is_quote {
            if next == Some(c) {
                self.docs[self.cur].cursor.1 = col + 1;
                return true;
            }
            if prev == Some('\\') {
                return false;
            }
            let word_len = line[..col].iter().rev().take_while(|ch| motion::is_word_char(**ch)).count();
            if word_len > 0 {
                let word: String = line[col - word_len..col].iter().collect::<String>().to_lowercase();
                let prefix = c != '`' && matches!(word.as_str(), "r" | "b" | "f" | "u" | "rb" | "br" | "fr" | "rf" | "ur" | "l" | "u8");
                if !prefix {
                    return false;
                }
            }
            if c == '\'' && matches!(prev, Some('&') | Some('<')) && self.is_rust() {
                return false;
            }
            // Python docstrings: `""` + `"` -> `"""|"""`
            if c != '`' && self.is_python_like() && col >= 2 && line[col - 1] == c && line[col - 2] == c && (col < 3 || line[col - 3] != c) {
                let text = [c, c, c, c];
                self.buf_mut().insert_text((r, col), &text);
                self.docs[self.cur].cursor = (r, col + 1);
                return true;
            }
        }
        let next_ok = match next {
            None => true,
            Some(n) if n.is_whitespace() => true,
            Some(')') | Some(']') | Some('}') | Some(',') | Some(';') | Some(':') | Some('>') => true,
            // directly inside a string: the next character is its closing quote
            Some(q @ ('"' | '\'' | '`')) => {
                let mut open = false;
                let mut esc = false;
                for ch in &line[..col] {
                    if esc {
                        esc = false;
                    } else if *ch == '\\' {
                        esc = true;
                    } else if *ch == q {
                        open = !open;
                    }
                }
                open
            }
            _ => false,
        };
        if !next_ok {
            return false;
        }
        self.buf_mut().insert_text((r, col), &[c, close]);
        self.docs[self.cur].cursor = (r, col + 1);
        true
    }

    fn insert_backspace(&mut self) {
        let (r, c) = self.cursor();
        if self.opts.autopairs && c > 0 {
            let line = &self.docs[self.cur].buf.lines[r];
            if let Some(&n) = line.get(c) {
                let p = line[c - 1];
                if matches!((p, n), ('(', ')') | ('[', ']') | ('{', '}') | ('"', '"') | ('\'', '\'') | ('`', '`')) {
                    // an empty auto-closed pair goes away as a whole
                    self.buf_mut().delete_range((r, c - 1), (r, c + 1));
                    self.docs[self.cur].cursor = (r, c - 1);
                    return;
                }
            }
        }
        if c > 0 {
            let (tabs, sw) = self.indent_style();
            let line = self.lines()[r].clone();
            let mut n = 1;
            if !tabs && line[..c].iter().all(|ch| *ch == ' ') && sw > 1 {
                n = c - ((c - 1) / sw) * sw;
            }
            self.buf_mut().delete_range((r, c - n), (r, c));
            self.docs[self.cur].cursor = (r, c - n);
        } else if r > 0 {
            let prev = self.line_len(r - 1);
            self.buf_mut().delete_range((r - 1, prev), (r, 0));
            self.docs[self.cur].cursor = (r - 1, prev);
        }
    }

    fn insert_delete_word(&mut self) {
        let (r, c) = self.cursor();
        if c == 0 {
            self.insert_backspace();
            return;
        }
        let line = self.lines()[r].clone();
        let mut i = c;
        while i > 0 && line[i - 1].is_whitespace() {
            i -= 1;
        }
        if i > 0 {
            let k = class(line[i - 1], false);
            while i > 0 && class(line[i - 1], false) == k {
                i -= 1;
            }
        }
        self.buf_mut().delete_range((r, i), (r, c));
        self.docs[self.cur].cursor = (r, i);
    }

    pub fn leave_insert(&mut self) {
        const REPEAT_BUDGET: usize = 4_000_000; // chars a repeated insert may add at once
        const REPLAY_KEY_BUDGET: usize = 20_000; // keys replayed one by one for a repeated insert
        self.completion = None;
        self.snippet = None;
        // `3ifoo<Esc>` inserts the text three times
        if self.insert.count > 1 && !self.insert.repeating {
            let keys = std::mem::take(&mut self.insert.keys);
            let open = self.insert.open_line;
            self.insert.repeating = true;
            self.replay_depth += 1;
            let was = self.in_dot;
            self.in_dot = true;
            let plain: Option<Vec<char>> = if open.is_none() {
                keys.iter()
                    .map(|k| match k {
                        Key::Char(c) if c.is_alphanumeric() || *c == ' ' || *c == '_' => Some(*c),
                        _ => None,
                    })
                    .collect()
            } else {
                None
            };
            if let Some(text) = plain {
                // plain words repeated: one insertion (typing them one by one would copy the
                // growing line for every character), capped so `99999999ix` cannot eat the memory
                let room = crate::buffer::max_chars().saturating_sub(self.buf().char_count());
                let times = (self.insert.count - 1).min(REPEAT_BUDGET / text.len().max(1)).min(room / text.len().max(1));
                if times > 0 {
                    let mut all = Vec::with_capacity(text.len() * times);
                    for _ in 0..times {
                        all.extend_from_slice(&text);
                    }
                    self.insert_text_at_cursor(&all);
                }
            } else {
                // anything else is replayed key by key; stop when that has done a lot of work
                let mut budget = REPLAY_KEY_BUDGET;
                'repeat: for _ in 1..self.insert.count {
                    if let Some(below) = open {
                        self.open_line(below);
                    }
                    for k in keys.iter().cloned() {
                        if budget == 0 || self.timed_out() || self.buf().char_count() > crate::buffer::max_chars() {
                            self.set_msg("repeat count cut short");
                            break 'repeat;
                        }
                        budget -= 1;
                        self.insert_key(k);
                    }
                }
            }
            self.in_dot = was;
            self.replay_depth -= 1;
        }
        self.mode = Mode::Normal;
        let (r, c) = self.cursor();
        if self.insert.ai_row == Some(r) && self.lines()[r].iter().all(|ch| ch.is_whitespace()) && !self.lines()[r].is_empty() {
            self.buf_mut().set_line(r, Vec::new());
            self.docs[self.cur].cursor = (r, 0);
        } else if c > 0 {
            self.docs[self.cur].cursor = (r, c - 1);
        }
        self.insert.ai_row = None;
        self.clamp_cursor();
        self.update_want();
        let here = self.cursor();
        self.doc_mut().marks.insert('^', here);
        self.finish_group();
        if !self.in_dot && !self.cur_change.is_empty() {
            self.last_change = std::mem::take(&mut self.cur_change);
        }
    }

    // ---- command line / search ---------------------------------------------

    pub fn start_cmdline(&mut self, kind: char, prefill: &str) {
        self.prev_mode = self.mode;
        self.mode = if kind == ':' { Mode::Command } else { Mode::Search };
        self.cmd_kind = kind;
        self.cmdline = prefill.to_string();
        self.hist_idx = None;
    }

    fn cancel_cmdline(&mut self) {
        self.search_op = None;
        self.cmdline.clear();
        self.mode = Mode::Normal;
        self.keybuf.clear();
    }

    fn cmdline_key(&mut self, k: Key) {
        match k {
            Key::Esc | Key::Ctrl('c') => self.cancel_cmdline(),
            Key::Enter => {
                let line = std::mem::take(&mut self.cmdline);
                let kind = self.cmd_kind;
                self.mode = Mode::Normal;
                self.keybuf.clear();
                if kind == ':' {
                    if !line.trim().is_empty() {
                        self.cmd_hist.push(line.clone());
                    }
                    self.execute_ex(&line);
                } else {
                    if !line.is_empty() {
                        self.search_hist.push(line.clone());
                    }
                    if let Some(sop) = self.search_op.take() {
                        self.finish_search_op(sop, &line, kind == '/');
                    } else {
                        self.run_search(&line, kind == '/');
                    }
                }
            }
            Key::Backspace | Key::Ctrl('h') => {
                if self.cmdline.is_empty() {
                    self.cancel_cmdline();
                } else {
                    self.cmdline.pop();
                }
            }
            Key::Ctrl('u') => self.cmdline.clear(),
            Key::Ctrl('w') => {
                let t = self.cmdline.trim_end().len();
                let s = &self.cmdline[..t];
                // (after the separator, which may be a multi-byte space)
                let cut = s.char_indices().rev().find(|(_, c)| c.is_whitespace() || *c == '/').map_or(0, |(i, c)| i + c.len_utf8());
                self.cmdline.truncate(cut);
            }
            Key::Up | Key::Ctrl('p') => self.history_step(-1),
            Key::Down | Key::Ctrl('n') => self.history_step(1),
            Key::Tab if self.cmd_kind == ':' => self.complete_cmdline(),
            Key::Char(c) => self.cmdline.push(c),
            Key::Paste(s) => self.cmdline.push_str(s.lines().next().unwrap_or("")),
            _ => {}
        }
    }

    fn history_step(&mut self, dir: isize) {
        let hist = if self.cmd_kind == ':' { &self.cmd_hist } else { &self.search_hist };
        if hist.is_empty() {
            return;
        }
        let n = hist.len() as isize;
        let cur = self.hist_idx.map_or(n, |i| i as isize);
        let next = cur + dir;
        if next < 0 {
            return;
        }
        if next >= n {
            self.hist_idx = None;
            self.cmdline.clear();
            return;
        }
        self.hist_idx = Some(next as usize);
        self.cmdline = hist[next as usize].clone();
    }

    fn complete_cmdline(&mut self) {
        let line = self.cmdline.clone();
        let (cmd, arg) = match line.split_once(' ') {
            Some((c, a)) => (c.to_string(), a.to_string()),
            None => return,
        };
        if !matches!(cmd.as_str(), "e" | "edit" | "w" | "write" | "r" | "sp" | "tabe") {
            return;
        }
        let (dir, prefix) = match arg.rfind('/') {
            Some(i) => (arg[..=i].to_string(), arg[i + 1..].to_string()),
            None => (String::new(), arg.clone()),
        };
        let read = if dir.is_empty() { ".".to_string() } else { dir.clone() };
        let mut names: Vec<String> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&read) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with(&prefix) && (prefix.starts_with('.') || !name.starts_with('.')) {
                    let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    names.push(if is_dir { format!("{}/", name) } else { name });
                }
            }
        }
        names.sort();
        if names.is_empty() {
            return;
        }
        // longest common prefix
        let mut common = names[0].clone();
        for n in &names[1..] {
            while !n.starts_with(&common) {
                common.pop();
            }
        }
        self.cmdline = format!("{} {}{}", cmd, dir, common);
        if names.len() > 1 {
            let shown: Vec<_> = names.iter().take(12).cloned().collect();
            self.set_msg(shown.join("  "));
        }
    }

    pub fn search_icase(&self, pat: &[char]) -> bool {
        self.opts.ignorecase && !(self.opts.smartcase && pat.iter().any(|c| c.is_uppercase()))
    }

    /// Find the next match from `from` in direction `forward`. Returns (pos, wrapped).
    pub fn find_match(&self, from: Pos, s: &Search, forward: bool) -> Option<(Pos, bool)> {
        let lines = self.lines();
        let n = lines.len();
        let icase = self.search_icase(&s.pat);
        if s.pat.is_empty() {
            return None;
        }
        if forward {
            if let Some(c) = find_in_line_fwd(&lines[from.0], &s.pat, from.1 + 1, icase, s.whole) {
                return Some(((from.0, c), false));
            }
            for (r, line) in lines.iter().enumerate().take(n).skip(from.0 + 1) {
                if let Some(c) = find_in_line_fwd(line, &s.pat, 0, icase, s.whole) {
                    return Some(((r, c), false));
                }
            }
            if self.opts.wrapscan {
                for (r, line) in lines.iter().enumerate().take(from.0 + 1) {
                    if let Some(c) = find_in_line_fwd(line, &s.pat, 0, icase, s.whole) {
                        if r < from.0 || c <= from.1 {
                            return Some(((r, c), true));
                        }
                    }
                }
            }
            None
        } else {
            if let Some(c) = find_in_line_bwd(&lines[from.0], &s.pat, from.1, icase, s.whole) {
                return Some(((from.0, c), false));
            }
            for r in (0..from.0).rev() {
                if let Some(c) = find_in_line_bwd(&lines[r], &s.pat, lines[r].len() + 1, icase, s.whole) {
                    return Some(((r, c), false));
                }
            }
            if self.opts.wrapscan {
                for r in (from.0..n).rev() {
                    if let Some(c) = find_in_line_bwd(&lines[r], &s.pat, lines[r].len() + 1, icase, s.whole) {
                        if r > from.0 || c >= from.1 {
                            return Some(((r, c), true));
                        }
                    }
                }
            }
            None
        }
    }

    pub fn run_search(&mut self, pat: &str, forward: bool) {
        if pat.is_empty() {
            if self.last_search.is_none() {
                return;
            }
        } else {
            self.last_search = Some(Search { pat: pat.chars().collect(), forward, whole: false });
        }
        if let Some(s) = self.last_search.as_mut() {
            s.forward = forward;
        }
        self.hl_search = true;
        self.search_step(false, 1);
    }

    /// `n` / `N`: move to the next match. Returns false when nothing matched.
    pub fn search_step(&mut self, reverse: bool, count: usize) -> bool {
        let s = match self.last_search.clone() {
            Some(s) => s,
            None => {
                self.set_err("No previous search pattern");
                return false;
            }
        };
        let forward = s.forward != reverse;
        let mut pos = self.cursor();
        let mut wrapped = false;
        for _ in 0..count.max(1) {
            if self.timed_out() {
                return false;
            }
            match self.find_match(pos, &s, forward) {
                Some((p, w)) => {
                    pos = p;
                    wrapped |= w;
                }
                None => {
                    let p: String = s.pat.iter().collect();
                    self.set_err(format!("Pattern not found: {}", p));
                    return false;
                }
            }
        }
        self.push_jump();
        self.move_to(pos);
        if wrapped {
            self.set_msg(if forward { "search hit BOTTOM, continuing at TOP" } else { "search hit TOP, continuing at BOTTOM" });
        } else {
            let p: String = s.pat.iter().collect();
            self.set_msg(format!("{}{}", if s.forward { '/' } else { '?' }, p));
        }
        true
    }

    // ---- placeholders filled in by other modules ---------------------------

    pub fn text_for_dump(&self) -> String {
        self.buf().text()
    }
}
