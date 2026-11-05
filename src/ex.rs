//! Ex commands (`:w`, `:s`, `:g`, ...).

use std::path::{Path, PathBuf};

use crate::buffer::Pos;
use crate::editor::{find_in_line_fwd, Doc, Editor, PickItem, PickTarget, Picker, Popup, Reg, Search};
use crate::help::HELP;
use crate::input::Key;

type Res = Result<(), String>;

struct Range {
    r0: usize,
    r1: usize,
    given: bool,
}

fn split_unescaped(s: &str, delim: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.peek() {
                Some(&n) if n == delim => {
                    cur.push(n);
                    it.next();
                }
                Some(&n) => {
                    cur.push('\\');
                    cur.push(n);
                    it.next();
                }
                None => cur.push('\\'),
            }
        } else if c == delim {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    parts.push(cur);
    parts
}

fn expand_replacement(rep: &str, matched: &[char]) -> Vec<char> {
    let mut out = Vec::new();
    let mut it = rep.chars();
    while let Some(c) = it.next() {
        match c {
            '&' => out.extend_from_slice(matched),
            '\\' => match it.next() {
                Some('n') | Some('r') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('&') => out.push('&'),
                Some('\\') => out.push('\\'),
                Some(o) => out.push(o),
                None => out.push('\\'),
            },
            c => out.push(c),
        }
    }
    out
}

pub fn skip_dir(name: &str) -> bool {
    matches!(name, ".git" | "target" | "node_modules" | ".hg" | ".svn" | "__pycache__" | ".venv" | "dist" | "build" | ".idea" | ".cache")
}

impl Editor {
    pub fn execute_ex(&mut self, line: &str) {
        let line = line.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
        if line.is_empty() {
            return;
        }
        self.last_ex = Some(line.to_string());
        let d = self.cur;
        let c = self.cursor();
        self.docs[d].buf.begin_group(c);
        let res = self.ex_inner(line);
        if d < self.docs.len() {
            let c2 = self.docs[d].cursor;
            self.docs[d].buf.end_group(c2);
        }
        if let Err(e) = res {
            self.set_err(e);
        }
        self.clamp_cursor();
        self.update_want();
    }

    // ---- ranges ----------------------------------------------------------

    fn parse_address(&mut self, s: &str) -> Result<(Option<usize>, usize), String> {
        // returns (row, bytes consumed)
        let b: Vec<char> = s.chars().collect();
        let last = self.buf().line_count() - 1;
        let mut i = 0;
        let mut base: Option<i64> = None;
        match b.first() {
            Some(c) if c.is_ascii_digit() => {
                let mut n = 0usize;
                while i < b.len() && b[i].is_ascii_digit() {
                    n = n.saturating_mul(10).saturating_add(b[i].to_digit(10).unwrap() as usize);
                    i += 1;
                }
                base = Some(n.min(1 << 40) as i64 - 1);
            }
            Some('.') => {
                base = Some(self.cursor().0 as i64);
                i = 1;
            }
            Some('$') => {
                base = Some(last as i64);
                i = 1;
            }
            Some('\'') if b.len() >= 2 => {
                let m = b[1];
                let p = self.doc().marks.get(&m).copied().ok_or_else(|| "Mark not set".to_string())?;
                base = Some(p.0 as i64);
                i = 2;
            }
            Some('/') => {
                let mut j = 1;
                let mut pat = String::new();
                while j < b.len() && b[j] != '/' {
                    if b[j] == '\\' && j + 1 < b.len() {
                        j += 1;
                    }
                    pat.push(b[j]);
                    j += 1;
                }
                if j < b.len() {
                    j += 1;
                }
                let sr = Search { pat: pat.chars().collect(), forward: true, whole: false };
                let from = (self.cursor().0, usize::MAX / 2);
                let from = (from.0, self.line_len(from.0));
                let (p, _) = self.find_match(from, &sr, true).ok_or_else(|| format!("Pattern not found: {}", pat))?;
                base = Some(p.0 as i64);
                i = j;
            }
            _ => {}
        }
        let mut off: i64 = 0;
        let mut had_off = false;
        while i < b.len() && (b[i] == '+' || b[i] == '-') {
            let sign = if b[i] == '-' { -1 } else { 1 };
            i += 1;
            let mut n = 0i64;
            let mut digits = false;
            while i < b.len() && b[i].is_ascii_digit() {
                n = n.saturating_mul(10).saturating_add(b[i].to_digit(10).unwrap() as i64).min(1 << 40);
                i += 1;
                digits = true;
            }
            off = off.saturating_add(sign * if digits { n } else { 1 });
            had_off = true;
        }
        if base.is_none() && !had_off {
            return Ok((None, 0));
        }
        let row = base.unwrap_or(self.cursor().0 as i64).saturating_add(off);
        if row < -1 || row > last as i64 {
            return Err("Invalid range".to_string());
        }
        let byte_len: usize = b[..i].iter().map(|c| c.len_utf8()).sum();
        Ok((Some(row.max(0) as usize), byte_len))
    }

    fn parse_range<'a>(&mut self, s: &'a str) -> Result<(Range, &'a str), String> {
        let cur = self.cursor().0;
        let last = self.buf().line_count() - 1;
        let s = s.trim_start();
        if let Some(rest) = s.strip_prefix('%') {
            return Ok((Range { r0: 0, r1: last, given: true }, rest));
        }
        let (a, n1) = self.parse_address(s)?;
        let mut rest = &s[n1..];
        let mut r0 = a;
        let mut r1 = a;
        if rest.starts_with(',') || rest.starts_with(';') {
            let semi = rest.starts_with(';');
            rest = &rest[1..];
            if semi {
                if let Some(r) = r0 {
                    self.docs[self.cur].cursor = (r, 0);
                }
            }
            let (b, n2) = self.parse_address(rest)?;
            rest = &rest[n2..];
            r1 = b.or(Some(self.cursor().0));
            if r0.is_none() {
                r0 = Some(cur);
            }
        }
        let given = r0.is_some();
        let (mut x, mut y) = (r0.unwrap_or(cur), r1.unwrap_or(r0.unwrap_or(cur)));
        if x > y {
            std::mem::swap(&mut x, &mut y);
        }
        Ok((Range { r0: x.min(last), r1: y.min(last), given }, rest))
    }

    // ---- dispatcher -----------------------------------------------------------

    fn ex_inner(&mut self, line: &str) -> Res {
        let (range, rest) = self.parse_range(line)?;
        let rest = rest.trim_start();
        // command name
        let mut chars = rest.char_indices();
        let (name, args): (String, &str) = match chars.next() {
            None => {
                // bare range: go to line
                if range.given {
                    let r = range.r1;
                    self.push_jump();
                    let c = self.first_nonblank(r);
                    self.move_to((r, c));
                }
                return Ok(());
            }
            Some((_, c)) if c.is_alphabetic() => {
                let end = rest.char_indices().find(|(_, ch)| !ch.is_alphabetic()).map_or(rest.len(), |(i, _)| i);
                (rest[..end].to_string(), &rest[end..])
            }
            Some((_, c)) if matches!(c, '<' | '>') => {
                let n = rest.chars().take_while(|x| *x == c).count();
                (c.to_string().repeat(n), &rest[n..])
            }
            Some((_, c)) => (c.to_string(), &rest[c.len_utf8()..]),
        };
        let bang = args.starts_with('!') && name != "!" && name != "&";
        let args = if bang { &args[1..] } else { args };
        let arg = args.trim();
        let cur = self.cursor().0;
        let rng = |this: &Editor, default_all: bool| -> (usize, usize) {
            if range.given {
                (range.r0, range.r1)
            } else if default_all {
                (0, this.buf().line_count() - 1)
            } else {
                (cur, cur)
            }
        };

        match name.as_str() {
            "w" | "write" => self.write_buffer(if arg.is_empty() { None } else { Some(arg) }, bang),
            "wq" | "x" | "xit" | "exit" => {
                self.write_buffer(if arg.is_empty() { None } else { Some(arg) }, bang)?;
                self.try_quit(false)
            }
            "wa" | "wall" => self.write_all(),
            "wqa" | "wqall" | "xa" | "xall" => {
                self.write_all()?;
                self.try_quit(false)
            }
            "q" | "quit" | "clo" | "close" => self.try_quit(bang),
            "qa" | "qall" | "quita" | "quitall" | "cq" => self.try_quit(bang),
            "e" | "edit" => {
                if arg.is_empty() {
                    self.reload_current(bang)
                } else {
                    let p = self.expand_path(arg);
                    let idx = self.open_file(&p)?;
                    self.switch_to(idx);
                    Ok(())
                }
            }
            "enew" | "ene" => {
                self.docs.push(Doc::new(crate::buffer::Buffer::new()));
                let i = self.docs.len() - 1;
                self.switch_to(i);
                Ok(())
            }
            "bn" | "bnext" => {
                let n = self.docs.len();
                let i = (self.cur + 1) % n;
                self.switch_to(i);
                Ok(())
            }
            "bp" | "bprev" | "bprevious" | "bN" | "bNext" => {
                let n = self.docs.len();
                let i = (self.cur + n - 1) % n;
                self.switch_to(i);
                Ok(())
            }
            "bd" | "bdelete" | "bw" | "bwipeout" | "bun" | "bunload" => self.delete_buffer(bang),
            "b" | "buffer" => {
                if arg.is_empty() {
                    if let Some(a) = self.alt {
                        self.switch_to(a);
                    }
                    return Ok(());
                }
                if let Ok(n) = arg.parse::<usize>() {
                    if n >= 1 && n <= self.docs.len() {
                        self.switch_to(n - 1);
                        return Ok(());
                    }
                    return Err(format!("No buffer {}", n));
                }
                let found = self.docs.iter().position(|d| d.name().contains(arg));
                match found {
                    Some(i) => {
                        self.switch_to(i);
                        Ok(())
                    }
                    None => Err(format!("No matching buffer for {}", arg)),
                }
            }
            "ls" | "buffers" | "files_" => {
                self.open_buffer_picker();
                Ok(())
            }
            "files" | "find" | "fin" => {
                self.open_file_picker();
                Ok(())
            }
            "grep" | "gr" | "vimgrep" => self.grep(arg),
            "set" | "se" => {
                for item in arg.split_whitespace() {
                    if let Some(q) = item.strip_suffix('?') {
                        match self.opts.describe(q) {
                            Some(s) => self.set_msg(s),
                            None => return Err(format!("unknown option: {}", q)),
                        }
                    } else {
                        self.opts.set(item)?;
                    }
                }
                Ok(())
            }
            "noh" | "nohlsearch" => {
                self.hl_search = false;
                Ok(())
            }
            "s" | "substitute" => {
                let (a, b) = rng(self, false);
                self.substitute(a, b, arg_raw(args))
            }
            "&" | "&&" => {
                let (a, b) = rng(self, false);
                self.substitute_repeat(a, b)
            }
            "d" | "delete" => {
                let (a, b) = rng(self, false);
                self.ex_delete_yank(a, b, true, arg);
                Ok(())
            }
            "y" | "yank" => {
                let (a, b) = rng(self, false);
                self.ex_delete_yank(a, b, false, arg);
                Ok(())
            }
            "pu" | "put" => {
                let r = if range.given { range.r1 } else { cur };
                if let Some(rg) = self.get_reg(arg.chars().next()) {
                    let lines: Vec<Vec<char>> = rg.text.split(|c| *c == '\n').map(|s| s.to_vec()).collect();
                    self.buf_mut().insert_lines(if bang { r } else { r + 1 }, lines);
                    let at = if bang { r } else { r + 1 };
                    self.move_to((at, 0));
                }
                Ok(())
            }
            ">" | ">>" | ">>>" | "<" | "<<" | "<<<" => {
                let (a, b) = rng(self, false);
                let lv = name.len() as isize * if name.starts_with('<') { -1 } else { 1 };
                for r in a..=b {
                    self.shift_line(r, lv);
                }
                let c = self.first_nonblank(b);
                self.move_to((b, c));
                Ok(())
            }
            "j" | "join" => {
                let (a, b) = rng(self, false);
                let n = if range.given && b > a { b - a + 1 } else { 2 };
                self.join_lines(a, n, !bang);
                Ok(())
            }
            "m" | "move" | "t" | "co" | "copy" => {
                let (a, b) = rng(self, false);
                let dest = {
                    let (rg, _) = self.parse_range(arg)?;
                    if arg.trim() == "0" { None } else { Some(rg.r1) }
                };
                let text: Vec<Vec<char>> = (a..=b).map(|r| self.lines()[r].clone()).collect();
                let at = dest.map_or(0, |d| d + 1);
                if name == "m" || name == "move" {
                    if at > a && at <= b + 1 {
                        return Ok(());
                    }
                    self.buf_mut().insert_lines(at, text.clone());
                    let (da, db) = if at > a { (a, b) } else { (a + text.len(), b + text.len()) };
                    self.buf_mut().delete_lines(da, db);
                    let land = if at > a { at - text.len() } else { at };
                    self.move_to((land + text.len() - 1, 0));
                } else {
                    self.buf_mut().insert_lines(at, text.clone());
                    self.move_to((at + text.len() - 1, 0));
                }
                Ok(())
            }
            "sort" | "sor" => {
                let (a, b) = rng(self, true);
                self.sort_lines(a, b, bang, arg);
                Ok(())
            }
            "normal" | "norm" => {
                let (a, b) = rng(self, false);
                self.ex_normal(a, b, args.strip_prefix(' ').unwrap_or(args), range.given);
                Ok(())
            }
            "g" | "global" | "v" | "vglobal" => {
                let (a, b) = rng(self, true);
                self.global(a, b, args, bang || name.starts_with('v'))
            }
            "reg" | "registers" | "di" | "display" => {
                self.show_registers();
                Ok(())
            }
            "marks" => {
                let mut lines = vec!["mark  line  col".to_string()];
                let mut ms: Vec<_> = self.doc().marks.iter().map(|(k, v)| (*k, *v)).collect();
                ms.sort();
                for (k, v) in ms {
                    lines.push(format!(" {}    {:>5} {:>4}", k, v.0 + 1, v.1));
                }
                self.popup = Some(Popup { title: "marks".into(), lines, scroll: 0 });
                Ok(())
            }
            "h" | "help" => {
                self.popup = Some(Popup { title: "help".into(), lines: HELP.lines().map(|s| s.to_string()).collect(), scroll: 0 });
                Ok(())
            }
            "cd" | "chdir" => {
                let target = if arg.is_empty() { std::env::var("HOME").unwrap_or_else(|_| ".".into()) } else { arg.to_string() };
                std::env::set_current_dir(&target).map_err(|e| format!("{}: {}", target, e))?;
                self.tree_new_root();
                self.set_msg(std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default());
                Ok(())
            }
            "pwd" | "pw" => {
                self.set_msg(std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default());
                Ok(())
            }
            "r" | "read" => {
                if let Some(cmd) = arg.strip_prefix('!') {
                    let out = run_shell(cmd)?;
                    let r = if range.given { range.r1 } else { cur };
                    let lines: Vec<Vec<char>> = out.trim_end_matches('\n').split('\n').map(|s| s.chars().collect()).collect();
                    self.buf_mut().insert_lines(r + 1, lines);
                    self.move_to((r + 1, 0));
                } else {
                    let p = self.expand_path(arg);
                    let text = crate::buffer::read_text(&p).map_err(|e| format!("{}: {}", p.display(), e))?;
                    let r = if range.given { range.r1 } else { cur };
                    let lines: Vec<Vec<char>> = text.trim_end_matches('\n').split('\n').map(|s| s.chars().collect()).collect();
                    self.buf_mut().insert_lines(r + 1, lines);
                    self.move_to((r + 1, 0));
                }
                Ok(())
            }
            "!" => {
                let out = run_shell(arg)?;
                let mut lines: Vec<String> = out.lines().map(|s| s.to_string()).collect();
                if lines.is_empty() {
                    lines.push("(no output)".into());
                }
                self.popup = Some(Popup { title: format!("!{}", arg), lines, scroll: 0 });
                Ok(())
            }
            "term" | "terminal" => {
                self.open_terminal();
                Ok(())
            }
            "termclose" | "termc" => {
                self.close_terminal();
                Ok(())
            }
            "termhide" => {
                self.hide_terminal();
                Ok(())
            }
            "tree" | "sidebar" | "explore" => {
                self.toggle_sidebar();
                Ok(())
            }
            "lsp" | "lspinfo" => {
                let s = self.lsp_status();
                self.popup = Some(Popup { title: "lsp".into(), lines: s.lines().map(|l| l.to_string()).collect(), scroll: 0 });
                Ok(())
            }
            "lsprestart" => {
                self.lsp_restart();
                Ok(())
            }
            "rename" => {
                if arg.is_empty() {
                    return Err("usage: :rename <new name>".into());
                }
                self.lsp_rename(arg)
            }
            "format" | "fmt" => self.lsp_format(),
            "diag" | "diagnostics" => {
                self.diag_picker();
                Ok(())
            }
            "refs" | "references" => {
                self.lsp_command("gr");
                Ok(())
            }
            "k" | "mark" | "ma" => {
                if let Some(c) = arg.chars().next() {
                    let r = if range.given { range.r1 } else { cur };
                    self.doc_mut().marks.insert(c, (r, 0));
                }
                Ok(())
            }
            "u" | "undo" => {
                if let Some(p) = self.buf_mut().undo() {
                    self.move_to(p);
                }
                Ok(())
            }
            "red" | "redo" => {
                if let Some(p) = self.buf_mut().redo() {
                    self.move_to(p);
                }
                Ok(())
            }
            "dump" => {
                self.set_msg(format!("{} lines", self.buf().line_count()));
                Ok(())
            }
            other => Err(format!("Not an editor command: {}", other)),
        }
    }

    // ---- files -------------------------------------------------------------------

    pub fn expand_path(&self, s: &str) -> PathBuf {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix("~/") {
            if let Ok(h) = std::env::var("HOME") {
                return PathBuf::from(h).join(rest);
            }
        }
        PathBuf::from(s)
    }

    fn write_buffer(&mut self, path: Option<&str>, force: bool) -> Res {
        let had_path = self.buf().path.is_some();
        let target: Option<PathBuf> = path.map(|p| self.expand_path(p));
        let lines = self.buf().line_count();
        let res = match &target {
            Some(p) => {
                let p = p.clone();
                self.buf_mut().save_opts(Some(&p), force).map(|n| (p, n))
            }
            None => {
                if self.buf().path.is_none() {
                    return Err("No file name".into());
                }
                let p = self.buf().path.clone().unwrap();
                self.buf_mut().save_opts(None, force).map(|n| (p, n))
            }
        };
        match res {
            Ok((p, n)) => {
                self.set_msg(format!("\"{}\" {}L, {}B written", p.display(), lines, n));
                if !had_path {
                    let i = self.cur;
                    self.lsp_attach(i);
                }
                self.lsp_saved();
                self.tree_refresh();
                Ok(())
            }
            Err(e) => Err(format!("write failed: {}", e)),
        }
    }

    fn write_all(&mut self) -> Res {
        let cur = self.cur;
        let mut n = 0;
        for i in 0..self.docs.len() {
            if self.docs[i].buf.modified && self.docs[i].buf.path.is_some() {
                self.cur = i;
                let r = self.docs[i].buf.save();
                self.cur = cur;
                r.map_err(|e| format!("{}: {}", self.docs[i].name(), e))?;
                n += 1;
            }
        }
        let saved_cur = self.cur;
        for i in 0..self.docs.len() {
            if self.docs[i].lsp.client.is_some() {
                self.cur = i;
                self.lsp_saved();
            }
        }
        self.cur = saved_cur;
        self.set_msg(format!("{} file(s) written", n));
        Ok(())
    }

    fn try_quit(&mut self, force: bool) -> Res {
        if !force {
            if let Some((i, d)) = self.docs.iter().enumerate().find(|(_, d)| d.buf.modified) {
                return Err(format!("No write since last change for buffer {} ({}) (add ! to override)", i + 1, d.name()));
            }
        }
        self.quit = true;
        Ok(())
    }

    fn reload_current(&mut self, force: bool) -> Res {
        if self.buf().modified && !force {
            return Err("No write since last change (add ! to override)".into());
        }
        let Some(p) = self.buf().path.clone() else { return Err("No file name".into()) };
        let nb = crate::buffer::Buffer::open(&p).map_err(|e| format!("{}: {}", p.display(), e))?;
        let cur = self.cursor();
        let d = self.doc_mut();
        let ver = d.buf.version + 1;
        d.buf = nb;
        d.buf.version = ver;
        self.move_to(cur);
        self.set_msg(format!("\"{}\" reloaded", p.display()));
        Ok(())
    }

    fn delete_buffer(&mut self, force: bool) -> Res {
        self.delete_buffer_at(self.cur, force)
    }

    /// Close buffer `idx` (the last one left becomes an empty buffer). Closing a buffer that is not
    /// the current one leaves the current one where it is.
    pub fn delete_buffer_at(&mut self, idx: usize, force: bool) -> Res {
        if idx >= self.docs.len() {
            return Err(format!("No buffer {}", idx + 1));
        }
        if self.docs[idx].buf.modified && !force {
            return Err("No write since last change (add ! to override)".into());
        }
        self.lsp_detach(idx);
        if idx == self.cur {
            self.snippet = None;
        }
        if self.docs.len() == 1 {
            self.docs[0] = Doc::new(crate::buffer::Buffer::new());
            self.alt = None;
            self.mode = crate::editor::Mode::Normal;
            return Ok(());
        }
        let was_current = idx == self.cur;
        self.docs.remove(idx);
        let fix = |i: usize| if i > idx { i - 1 } else { i };
        self.jumps.retain(|(d, _)| *d != idx);
        for j in self.jumps.iter_mut() {
            j.0 = fix(j.0);
        }
        self.jump_idx = self.jump_idx.min(self.jumps.len());
        self.alt = self.alt.filter(|a| *a != idx).map(fix);
        if was_current {
            let next = self.alt.unwrap_or(0).min(self.docs.len() - 1);
            self.cur = next;
            self.mode = crate::editor::Mode::Normal;
        } else {
            self.cur = fix(self.cur).min(self.docs.len() - 1);
        }
        self.clamp_cursor();
        Ok(())
    }

    pub fn open_buffer_picker(&mut self) {
        let items: Vec<PickItem> = self
            .docs
            .iter()
            .enumerate()
            .map(|(i, d)| PickItem {
                label: format!("{:>2} {}{} {}", i + 1, if i == self.cur { "%" } else { " " }, if d.buf.modified { "+" } else { " " }, d.name()),
                target: PickTarget::Buffer(i),
            })
            .collect();
        self.picker = Some(Picker::new("buffers", items));
    }

    /// All project files (git-aware when possible).
    fn project_files(&self) -> Vec<String> {
        // `git` found only in absolute PATH entries, bounded in time and size, and with the
        // repository's `core.fsmonitor` hook switched off (a hook is a program the project could name)
        if let Some(git) = crate::lsp::which("git") {
            let mut c = std::process::Command::new(git);
            c.args(["-c", "core.fsmonitor=false", "ls-files", "-co", "--exclude-standard"]).env("GIT_OPTIONAL_LOCKS", "0");
            if let Ok(o) = run_command_limited(c, std::time::Duration::from_secs(5), 32 << 20) {
                if o.status.success() && !o.stdout.is_empty() {
                    return String::from_utf8_lossy(&o.stdout).lines().map(|s| s.to_string()).filter(|p| Path::new(p).is_file()).collect();
                }
            }
        }
        // No `git` (or not a repository): walk the directory ourselves, bounded. Symbolic links are
        // not followed (a link back to a parent would otherwise multiply the work at every level),
        // and the number of directories, files and the time spent are limited.
        let mut out = Vec::new();
        let start = std::time::Instant::now();
        let mut dirs_left = 20_000usize;
        let mut stack: Vec<(PathBuf, usize)> = vec![(PathBuf::from("."), 0)];
        while let Some((dir, depth)) = stack.pop() {
            if dirs_left == 0 || start.elapsed() > std::time::Duration::from_secs(2) {
                break;
            }
            dirs_left -= 1;
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten().take(100_000) {
                let name = e.file_name().to_string_lossy().to_string();
                let Ok(ft) = e.file_type() else { continue };
                if name.starts_with('.') || skip_dir(&name) && ft.is_dir() {
                    continue;
                }
                let p = e.path();
                if ft.is_dir() {
                    if depth < 10 {
                        stack.push((p, depth + 1));
                    }
                } else if (ft.is_file() || ft.is_symlink() && p.is_file()) && out.len() < 50_000 {
                    out.push(p.strip_prefix("./").unwrap_or(&p).to_string_lossy().to_string());
                }
            }
        }
        out.sort();
        out
    }

    pub fn open_file_picker(&mut self) {
        let files = self.project_files();
        if files.is_empty() {
            self.set_err("No files found");
            return;
        }
        let items = files.into_iter().map(|f| PickItem { target: PickTarget::Location { path: PathBuf::from(&f), pos: (0, 0) }, label: f }).collect();
        self.picker = Some(Picker::new("files", items));
    }

    fn grep(&mut self, pat: &str) -> Res {
        if pat.is_empty() {
            return Err("usage: :grep <text>".into());
        }
        let needle: Vec<char> = pat.chars().collect();
        let icase = self.search_icase(&needle);
        let mut items = Vec::new();
        'files: for f in self.project_files() {
            let Ok(meta) = std::fs::metadata(&f) else { continue };
            if meta.len() > 2_000_000 {
                continue;
            }
            let Ok(text) = crate::buffer::read_text(Path::new(&f)) else { continue };
            for (i, l) in text.lines().enumerate() {
                let lc: Vec<char> = l.chars().collect();
                if let Some(col) = find_in_line_fwd(&lc, &needle, 0, icase, false) {
                    items.push(PickItem {
                        label: format!("{}:{}: {}", f, i + 1, l.trim()),
                        target: PickTarget::Location { path: PathBuf::from(&f), pos: (i, col) },
                    });
                    if items.len() >= 2000 {
                        break 'files;
                    }
                }
            }
        }
        if items.is_empty() {
            return Err(format!("No matches for: {}", pat));
        }
        self.picker = Some(Picker::new(&format!("grep: {}", pat), items));
        Ok(())
    }

    // ---- editing commands ------------------------------------------------------------

    fn ex_delete_yank(&mut self, a: usize, b: usize, delete: bool, arg: &str) {
        let reg = arg.chars().next().filter(|c| !c.is_ascii_digit());
        let text: Vec<char> = {
            let mut t = Vec::new();
            for r in a..=b {
                if r > a {
                    t.push('\n');
                }
                t.extend_from_slice(&self.lines()[r]);
            }
            t
        };
        self.set_reg(reg, text, true, !delete);
        if delete {
            self.buf_mut().delete_lines(a, b);
            let r = a.min(self.buf().line_count() - 1);
            let c = self.first_nonblank(r);
            self.move_to((r, c));
        }
    }

    fn sort_lines(&mut self, a: usize, b: usize, reverse: bool, arg: &str) {
        let numeric = arg.contains('n');
        let icase = arg.contains('i');
        let unique = arg.contains('u');
        let mut lines: Vec<Vec<char>> = (a..=b).map(|r| self.lines()[r].clone()).collect();
        let key = |l: &Vec<char>| -> String {
            let s: String = l.iter().collect();
            if icase {
                s.to_lowercase()
            } else {
                s
            }
        };
        if numeric {
            let num = |l: &Vec<char>| -> i64 {
                let s: String = l.iter().collect();
                let digits: String = s.chars().skip_while(|c| !c.is_ascii_digit() && *c != '-').take_while(|c| c.is_ascii_digit() || *c == '-').collect();
                digits.parse().unwrap_or(i64::MIN)
            };
            lines.sort_by_key(|l| num(l));
        } else {
            lines.sort_by_key(|l| key(l));
        }
        if reverse {
            lines.reverse();
        }
        if unique {
            lines.dedup_by(|x, y| key(x) == key(y));
        }
        self.buf_mut().replace_line_range(a, b, lines);
        self.move_to((a, 0));
    }

    fn ex_normal(&mut self, a: usize, b: usize, keys: &str, ranged: bool) {
        let ks: Vec<Key> = keys.chars().map(|c| if c == '\x1b' { Key::Esc } else { Key::Char(c) }).collect();
        let run = |this: &mut Editor, ks: Vec<Key>| {
            this.keybuf.clear();
            this.replay_keys(ks, false);
            if this.mode == crate::editor::Mode::Insert {
                this.leave_insert();
            }
            this.keybuf.clear();
        };
        if ranged {
            let mut r = b as isize;
            while r >= a as isize && !self.timed_out() {
                if (r as usize) < self.buf().line_count() {
                    self.docs[self.cur].cursor = (r as usize, 0);
                    run(self, ks.clone());
                }
                r -= 1;
            }
        } else {
            run(self, ks);
        }
    }

    fn global(&mut self, a: usize, b: usize, args: &str, invert: bool) -> Res {
        let args = args.trim_start();
        let delim = args.chars().next().ok_or("usage: :g/pattern/command")?;
        if delim.is_alphanumeric() {
            return Err("usage: :g/pattern/command".into());
        }
        let parts = split_unescaped(&args[delim.len_utf8()..], delim);
        let pat: Vec<char> = parts[0].chars().collect();
        let cmd = if parts.len() > 1 { parts[1..].join(&delim.to_string()) } else { String::new() };
        let cmd = if cmd.trim().is_empty() { "p".to_string() } else { cmd };
        let pat = if pat.is_empty() { self.last_search.as_ref().map(|s| s.pat.clone()).unwrap_or_default() } else { pat };
        if pat.is_empty() {
            return Err("No previous search pattern".into());
        }
        let icase = self.search_icase(&pat);
        let rows: Vec<usize> = (a..=b).filter(|&r| find_in_line_fwd(&self.lines()[r], &pat, 0, icase, false).is_some() != invert).collect();
        if rows.is_empty() {
            return Err(format!("Pattern not found: {}", pat.iter().collect::<String>()));
        }
        self.last_search = Some(Search { pat, forward: true, whole: false });
        if cmd.trim() == "p" {
            let lines: Vec<String> = rows.iter().map(|r| format!("{:>5}  {}", r + 1, self.buf().line_string(*r))).collect();
            self.popup = Some(Popup { title: "global".into(), lines, scroll: 0 });
            return Ok(());
        }
        let n = rows.len();
        for r in rows.into_iter().rev() {
            if self.timed_out() {
                break;
            }
            if r < self.buf().line_count() {
                self.docs[self.cur].cursor = (r, 0);
                self.ex_inner(&cmd)?;
            }
        }
        self.set_msg(format!("{} line(s) matched", n));
        Ok(())
    }

    fn show_registers(&mut self) {
        let mut names: Vec<char> = self.regs.keys().copied().filter(|c| *c != '\u{1}').collect();
        names.sort();
        let mut lines = vec!["reg  content".to_string()];
        for n in names {
            let r = &self.regs[&n];
            let mut s: String = r.text.iter().take(60).map(|c| if *c == '\n' { '\u{23ce}' } else { *c }).collect();
            if r.linewise {
                s.push('\u{23ce}');
            }
            lines.push(format!("\"{}   {}", n, s));
        }
        self.popup = Some(Popup { title: "registers".into(), lines, scroll: 0 });
    }

    // ---- substitute ---------------------------------------------------------------------

    fn substitute_repeat(&mut self, a: usize, b: usize) -> Res {
        let last = self.last_sub.clone().ok_or("No previous substitute")?;
        self.do_substitute(a, b, &last.0, &last.1, &last.2)
    }

    fn substitute(&mut self, a: usize, b: usize, args: &str) -> Res {
        let args = args.trim_start();
        let delim = match args.chars().next() {
            Some(c) if !c.is_alphanumeric() && c != '\\' && c != '"' && c != '|' && !c.is_whitespace() => c,
            _ => {
                if args.is_empty() {
                    return self.substitute_repeat(a, b);
                }
                return Err("usage: :s/pattern/replacement/[flags]".into());
            }
        };
        let parts = split_unescaped(&args[delim.len_utf8()..], delim);
        let pat = parts.first().cloned().unwrap_or_default();
        let rep = parts.get(1).cloned().unwrap_or_default();
        let flags = parts.get(2).cloned().unwrap_or_default();
        self.do_substitute(a, b, &pat, &rep, &flags)
    }

    fn do_substitute(&mut self, a: usize, b: usize, pat: &str, rep: &str, flags: &str) -> Res {
        let pat_chars: Vec<char> = if pat.is_empty() {
            self.last_search.as_ref().map(|s| s.pat.clone()).ok_or("No previous search pattern")?
        } else {
            pat.chars().collect()
        };
        if flags.contains('c') {
            return Err("the 'c' flag is not supported".into());
        }
        let global = flags.contains('g');
        let icase = if flags.contains('I') {
            false
        } else if flags.contains('i') {
            true
        } else {
            self.search_icase(&pat_chars)
        };
        self.last_sub = Some((pat.to_string(), rep.to_string(), flags.to_string()));
        self.last_search = Some(Search { pat: pat_chars.clone(), forward: true, whole: false });
        self.hl_search = true;
        let mut end = b;
        let mut r = a;
        let mut subs = 0usize;
        let mut lines_changed = 0usize;
        let mut last_row = None;
        let limit = crate::buffer::max_chars();
        let mut size = self.buf().char_count();
        while r <= end {
            let line = self.lines()[r].clone();
            let mut out: Vec<char> = Vec::new();
            let mut i = 0;
            let mut hit = false;
            while i <= line.len() {
                match find_in_line_fwd(&line, &pat_chars, i, icase, false) {
                    Some(m) => {
                        out.extend_from_slice(&line[i..m]);
                        out.extend(expand_replacement(rep, &line[m..m + pat_chars.len()]));
                        if size.saturating_add(out.len()) > limit {
                            return Err(format!("substitution stopped: the buffer would pass {} Mi characters", limit >> 20));
                        }
                        i = m + pat_chars.len();
                        subs += 1;
                        hit = true;
                        if !global {
                            break;
                        }
                    }
                    None => break,
                }
            }
            if hit {
                out.extend_from_slice(&line[i.min(line.len())..]);
                size = size.saturating_add(out.len()).saturating_sub(line.len());
                let pieces: Vec<Vec<char>> = out.split(|c| *c == '\n').map(|s| s.to_vec()).collect();
                let extra = pieces.len() - 1;
                let mut it = pieces.into_iter();
                self.buf_mut().set_line(r, it.next().unwrap());
                let rest: Vec<Vec<char>> = it.collect();
                if !rest.is_empty() {
                    self.buf_mut().insert_lines(r + 1, rest);
                }
                end += extra;
                lines_changed += 1;
                last_row = Some(r + extra);
                r += extra;
            }
            r += 1;
        }
        match last_row {
            Some(row) => {
                let c = self.first_nonblank(row);
                self.move_to((row, c));
                if subs > 1 || lines_changed > 1 {
                    self.set_msg(format!("{} substitution(s) on {} line(s)", subs, lines_changed));
                }
                Ok(())
            }
            None => Err(format!("Pattern not found: {}", pat_chars.iter().collect::<String>())),
        }
    }
}

fn arg_raw(args: &str) -> &str {
    args
}

/// Longest a `:!` / `:r !` / filter command may run, and the most output kept from it. The editor
/// cannot be interrupted from the keyboard while it waits, so a command that never ends (or never
/// stops printing) must not be able to hold it, or its memory, for ever.
const SHELL_TIME_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);
const SHELL_OUTPUT_LIMIT: usize = 16 << 20;

fn run_shell(cmd: &str) -> Result<String, String> {
    run_shell_limited(cmd, SHELL_TIME_LIMIT, SHELL_OUTPUT_LIMIT)
}

fn run_shell_limited(cmd: &str, time_limit: std::time::Duration, output_limit: usize) -> Result<String, String> {
    // an absolute path, so that a file called `sh` in the project (or a `.` in PATH) is never run instead
    let mut c = std::process::Command::new("/bin/sh");
    c.arg("-c").arg(cmd);
    let ran = run_command_limited(c, time_limit, output_limit)?;
    let mut s = String::from_utf8_lossy(&ran.stdout).to_string();
    s.push_str(&String::from_utf8_lossy(&ran.stderr));
    Ok(s)
}

/// What a finished helper program produced.
pub(crate) struct Ran {
    pub status: std::process::ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Run `cmd` (no input, its own process group) for at most `time_limit`, keeping at most
/// `output_limit` bytes of each output stream. A program that hangs or never stops printing is
/// stopped together with everything it started, and the caller gets an error instead.
pub(crate) fn run_command_limited(mut cmd: std::process::Command, time_limit: std::time::Duration, output_limit: usize) -> Result<Ran, String> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0).spawn().map_err(|e| e.to_string())?;
    let too_much = Arc::new(AtomicBool::new(false));
    let reader = move |pipe: Option<Box<dyn Read + Send>>, flag: Arc<AtomicBool>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(p) = pipe {
                let _ = p.take(output_limit as u64 + 1).read_to_end(&mut buf);
            }
            if buf.len() > output_limit {
                flag.store(true, Ordering::SeqCst);
                buf.truncate(output_limit);
            }
            buf
        })
    };
    let out_t = reader(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>), too_much.clone());
    let err_t = reader(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>), too_much.clone());
    let end = std::time::Instant::now() + time_limit;
    let mut problem: Option<String> = None;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(e) => {
                problem = Some(e.to_string());
                break;
            }
        }
        if too_much.load(Ordering::SeqCst) {
            problem = Some(format!("command stopped: more than {} KiB of output", output_limit >> 10));
            break;
        }
        if std::time::Instant::now() > end {
            problem = Some(format!("command stopped: it ran for more than {} s", time_limit.as_secs().max(1)));
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    if problem.is_none() {
        // the command is done, but something it started (`sleep 100 &`) may still hold the pipes
        // open: give the readers a moment, then end the stragglers rather than wait for them
        let grace = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while !(out_t.is_finished() && err_t.is_finished()) && std::time::Instant::now() < grace {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    // the command and whatever it started (only while something is still running: the number of a
    // finished and reaped process may already belong to a stranger)
    if problem.is_some() || !(out_t.is_finished() && err_t.is_finished()) {
        crate::sys::kill_group(child.id());
    }
    if problem.is_some() {
        let _ = child.kill();
    }
    let status = child.wait();
    let out = out_t.join().unwrap_or_default();
    let err = err_t.join().unwrap_or_default();
    // (the reader may have stopped at the cap and let the command die of SIGPIPE before we looked)
    if problem.is_none() && too_much.load(Ordering::SeqCst) {
        problem = Some(format!("command stopped: more than {} KiB of output", output_limit >> 10));
    }
    if let Some(p) = problem {
        return Err(p);
    }
    let status = status.map_err(|e| e.to_string())?;
    Ok(Ran { status, stdout: out, stderr: err })
}

#[allow(dead_code)]
fn _unused(_: Pos, _: Reg) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_helper_program_reports_its_status_and_output() {
        let mut c = std::process::Command::new("/bin/sh");
        c.args(["-c", "printf out; printf err >&2; exit 3"]);
        let r = run_command_limited(c, Duration::from_secs(10), 1 << 20).unwrap();
        assert_eq!(r.status.code(), Some(3));
        assert_eq!((r.stdout.as_slice(), r.stderr.as_slice()), (&b"out"[..], &b"err"[..]));
        // the shell wrapper never runs a program called `sh` from the current directory
        let r = run_shell_limited("echo ok", Duration::from_secs(10), 1 << 20).unwrap();
        assert_eq!(r, "ok\n");
    }

    #[test]
    fn a_command_that_runs_too_long_is_stopped() {
        let t = Instant::now();
        let r = run_shell_limited("sleep 30", Duration::from_millis(300), 1 << 20);
        assert!(r.unwrap_err().contains("ran for more than"), "wrong error");
        assert!(t.elapsed() < Duration::from_secs(5), "took {:?}", t.elapsed());
    }

    #[test]
    fn a_command_with_endless_output_is_stopped_at_the_cap() {
        let r = run_shell_limited("yes", Duration::from_secs(20), 64 << 10);
        assert!(r.unwrap_err().contains("KiB of output"));
    }

    #[test]
    fn ordinary_commands_return_their_output_and_stragglers_do_not_hold_us() {
        assert_eq!(run_shell_limited("printf 'a\\nb\\n'", Duration::from_secs(5), 1 << 20).unwrap(), "a\nb\n");
        let t = Instant::now();
        // a background process keeps the pipes open for a minute; we must not wait for it
        let r = run_shell_limited("echo done; sleep 60 &", Duration::from_secs(20), 1 << 20);
        assert_eq!(r.unwrap(), "done\n");
        assert!(t.elapsed() < Duration::from_secs(5), "waited {:?} for a background job", t.elapsed());
    }
}
