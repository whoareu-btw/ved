//! LSP snippets: `foo(${1:first}, ${2:second})$0`.
//!
//! Accepting a completion that carries a snippet inserts its text and starts a *session*: the first
//! placeholder is highlighted, typing replaces it, `Tab` / `Shift-Tab` jump to the next / previous one,
//! and the cursor ends up at `$0` (or just after the text). Typing is tracked key by key, so the
//! remaining placeholders keep their place while you edit.
//!
//! Not supported: placeholders that stay in sync with their mirrors (`$1 ... $1` is expanded once),
//! variables such as `$TM_FILENAME` (their default text is used), transformations.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::buffer::Pos;
use crate::editor::{Editor, Mode};
use crate::input::Key;

// ---------------------------------------------------------------------------------------
// parsing

#[derive(Clone, Debug, PartialEq)]
pub struct SnipStop {
    pub index: usize,
    /// offsets in chars into `Snip::text`
    pub start: usize,
    pub len: usize,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Snip {
    pub text: String,
    /// first occurrence of every tab stop, in the order they appear in the text
    pub stops: Vec<SnipStop>,
}

struct Parser {
    c: Vec<char>,
    i: usize,
    text: Vec<char>,
    stops: Vec<SnipStop>,
    defaults: HashMap<usize, Vec<char>>,
    depth: usize,
}

impl Parser {
    fn peek(&self, k: usize) -> Option<char> {
        self.c.get(self.i + k).copied()
    }

    fn number(&mut self) -> usize {
        let mut n = 0usize;
        while let Some(d) = self.peek(0).and_then(|ch| ch.to_digit(10)) {
            n = n.saturating_mul(10).saturating_add(d as usize);
            self.i += 1;
        }
        n
    }

    /// Text up to the end of the input, or (inside a placeholder) up to its closing `}`.
    fn seq(&mut self, in_placeholder: bool) {
        while let Some(ch) = self.peek(0) {
            match ch {
                '}' if in_placeholder => return,
                '\\' => match self.peek(1) {
                    Some(n @ ('$' | '}' | '\\')) => {
                        self.text.push(n);
                        self.i += 2;
                    }
                    _ => {
                        self.text.push('\\');
                        self.i += 1;
                    }
                },
                '$' => self.dollar(),
                _ => {
                    self.text.push(ch);
                    self.i += 1;
                }
            }
        }
    }

    /// A `${n:...}` / `${n|...|}` whose text starts at `start` has just been read.
    fn register(&mut self, index: usize, start: usize) {
        let len = self.text.len() - start;
        // nested placeholders are flattened into their parent; a repeated index is only a mirror
        if self.depth == 0 && !self.stops.iter().any(|s| s.index == index) {
            self.stops.push(SnipStop { index, start, len });
        }
        let seen = self.text[start..].to_vec();
        self.defaults.entry(index).or_insert(seen);
    }

    fn dollar(&mut self) {
        match self.peek(1) {
            Some(d) if d.is_ascii_digit() => {
                self.i += 1;
                let n = self.number();
                self.tabstop(n);
            }
            Some('{') => {
                self.i += 2;
                if self.peek(0).is_some_and(|d| d.is_ascii_digit()) {
                    let n = self.number();
                    match self.peek(0) {
                        Some(':') => {
                            self.i += 1;
                            let start = self.text.len();
                            self.depth += 1;
                            self.seq(true);
                            self.depth -= 1;
                            if self.peek(0) == Some('}') {
                                self.i += 1;
                            }
                            self.register(n, start);
                        }
                        Some('|') => {
                            self.i += 1;
                            let start = self.text.len();
                            let mut first = true;
                            while let Some(ch) = self.peek(0) {
                                if ch == '|' && self.peek(1) == Some('}') {
                                    self.i += 2;
                                    break;
                                }
                                self.i += 1;
                                if ch == ',' {
                                    first = false;
                                } else if first {
                                    self.text.push(ch);
                                }
                            }
                            self.register(n, start);
                        }
                        _ => {
                            if self.peek(0) == Some('}') {
                                self.i += 1;
                            }
                            self.tabstop(n);
                        }
                    }
                } else {
                    // ${name} or ${name:default}: variables are unknown here, use the default
                    while self.peek(0).is_some_and(|ch| ch.is_alphanumeric() || ch == '_') {
                        self.i += 1;
                    }
                    if self.peek(0) == Some(':') {
                        self.i += 1;
                        self.depth += 1;
                        self.seq(true);
                        self.depth -= 1;
                    }
                    if self.peek(0) == Some('}') {
                        self.i += 1;
                    }
                }
            }
            Some(a) if a.is_alphabetic() || a == '_' => {
                self.i += 1;
                while self.peek(0).is_some_and(|ch| ch.is_alphanumeric() || ch == '_') {
                    self.i += 1;
                }
            }
            _ => {
                self.text.push('$');
                self.i += 1;
            }
        }
    }

    /// `$n` / `${n}`: the first one is a stop; later ones repeat the first one's text.
    fn tabstop(&mut self, n: usize) {
        if self.depth == 0 && !self.stops.iter().any(|s| s.index == n) {
            self.stops.push(SnipStop { index: n, start: self.text.len(), len: 0 });
            self.defaults.entry(n).or_default();
        } else if let Some(d) = self.defaults.get(&n).cloned() {
            self.text.extend(d);
        }
    }
}

pub fn parse_snippet(src: &str) -> Snip {
    let mut p = Parser { c: src.chars().collect(), i: 0, text: Vec::new(), stops: Vec::new(), defaults: HashMap::new(), depth: 0 };
    p.seq(false);
    Snip { text: p.text.iter().collect(), stops: p.stops }
}

/// Lines after the first get the indentation of the line the snippet goes into, and the tabs
/// snippets use for their own nesting become one `unit` each.
pub fn adjust_indent(src: &str, indent: &str, unit: &str) -> String {
    if !src.contains('\n') {
        return src.replace('\r', "");
    }
    let src = src.replace("\r\n", "\n");
    let mut out = String::with_capacity(src.len() + 16);
    for (n, line) in src.split('\n').enumerate() {
        if n == 0 {
            out.push_str(line);
            continue;
        }
        out.push('\n');
        let tabs = line.chars().take_while(|c| *c == '\t').count();
        let rest = &line[tabs..];
        if rest.is_empty() {
            continue; // blank lines stay blank
        }
        out.push_str(indent);
        for _ in 0..tabs {
            out.push_str(unit);
        }
        out.push_str(rest);
    }
    out
}

// ---------------------------------------------------------------------------------------
// session

#[derive(Clone, Debug, PartialEq)]
pub struct Stop {
    pub index: usize,
    pub start: Pos,
    /// exclusive; on the same row as `start`
    pub end: Pos,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct Session {
    pub id: u64,
    /// in the order they appear in the text
    pub stops: Vec<Stop>,
    /// visiting order (indices into `stops`): 1, 2, 3 ... then 0
    pub order: Vec<usize>,
    pub cur: usize,
    /// the current placeholder is selected: typing replaces it
    pub sel: bool,
}

fn pos_in(base: Pos, text: &[char], offset: usize) -> Pos {
    let (mut r, mut c) = base;
    for ch in text.iter().take(offset) {
        if *ch == '\n' {
            r += 1;
            c = 0;
        } else {
            c += 1;
        }
    }
    (r, c)
}

impl Session {
    /// A session for `snip` inserted at `base`, or None when there is nothing to visit besides the
    /// final cursor position.
    pub fn new(snip: &Snip, base: Pos) -> Option<Session> {
        let text: Vec<char> = snip.text.chars().collect();
        let end = pos_in(base, &text, text.len());
        let mut stops: Vec<Stop> = snip
            .stops
            .iter()
            .map(|s| {
                let start = pos_in(base, &text, s.start);
                let mut stop_end = pos_in(base, &text, s.start + s.len);
                if stop_end.0 != start.0 {
                    stop_end = start; // a placeholder spanning lines is left alone
                }
                Stop { index: s.index, start, end: stop_end }
            })
            .collect();
        if !stops.iter().any(|s| s.index == 0) {
            stops.push(Stop { index: 0, start: end, end });
        }
        let mut order: Vec<usize> = (0..stops.len()).filter(|i| stops[*i].index != 0).collect();
        order.sort_by_key(|i| stops[*i].index);
        order.extend((0..stops.len()).filter(|i| stops[*i].index == 0));
        if order.len() < 2 {
            return None;
        }
        let first = &stops[order[0]];
        let sel = first.end > first.start;
        Some(Session { id: NEXT_ID.fetch_add(1, Ordering::Relaxed), stops, order, cur: 0, sel })
    }

    /// Where the cursor goes for a snippet that has no placeholders to visit.
    pub fn final_pos(snip: &Snip, base: Pos) -> Pos {
        let text: Vec<char> = snip.text.chars().collect();
        match snip.stops.iter().find(|s| s.index == 0) {
            Some(s) => pos_in(base, &text, s.start),
            None => pos_in(base, &text, text.len()),
        }
    }

    pub fn current(&self) -> &Stop {
        &self.stops[self.order[self.cur]]
    }

    /// `len` chars were typed at `at`, inside the current placeholder (or at its end).
    pub fn insert_at(&mut self, at: Pos, len: usize) {
        let cur = self.order[self.cur];
        for (i, s) in self.stops.iter_mut().enumerate() {
            if i < cur {
                continue; // written before the current one: not affected
            }
            if i > cur && s.start.0 == at.0 && s.start.1 >= at.1 {
                s.start.1 += len;
            }
            if s.end.0 == at.0 && s.end.1 >= at.1 {
                s.end.1 += len;
            }
        }
    }

    /// Chars `a..b` of `row` were deleted.
    pub fn delete_in_row(&mut self, row: usize, a: usize, b: usize) {
        let fix = |p: &mut Pos| {
            if p.0 == row {
                if p.1 >= b {
                    p.1 -= b - a;
                } else if p.1 > a {
                    p.1 = a;
                }
            }
        };
        for s in self.stops.iter_mut() {
            fix(&mut s.start);
            fix(&mut s.end);
        }
    }

    /// Enter at `at`: the rest of the row moved to the next row, starting at column `new_col`.
    pub fn split_row(&mut self, at: Pos, new_col: usize) {
        let mv = |p: Pos| -> Pos {
            if p.0 == at.0 && p.1 >= at.1 {
                (p.0 + 1, p.1 - at.1 + new_col)
            } else if p.0 > at.0 {
                (p.0 + 1, p.1)
            } else {
                p
            }
        };
        for s in self.stops.iter_mut() {
            // a placeholder that ends exactly where the row was split stays on the old row
            let stays = s.end == at && s.start < at;
            s.start = mv(s.start);
            if !stays {
                s.end = mv(s.end);
            }
            if s.end.0 != s.start.0 {
                s.end = (s.start.0, at.1); // split in the middle: keep the part before the break
            }
        }
    }

    /// Backspace at the start of `row` joined it onto the row above, whose length was `prev_len`.
    pub fn join_row(&mut self, row: usize, prev_len: usize) {
        let fix = |p: &mut Pos| {
            if p.0 == row {
                *p = (row - 1, prev_len + p.1);
            } else if p.0 > row {
                p.0 -= 1;
            }
        };
        for s in self.stops.iter_mut() {
            fix(&mut s.start);
            fix(&mut s.end);
        }
    }
}

// ---------------------------------------------------------------------------------------
// editor integration

pub struct Snap {
    id: u64,
    cursor: Pos,
    nlines: usize,
    len: usize,
}

impl Editor {
    /// Start a session for a snippet that was just inserted at `base`, and place the cursor.
    pub fn snippet_begin(&mut self, snip: &Snip, base: Pos) {
        self.snippet = None;
        let at = match Session::new(snip, base) {
            Some(s) => {
                let at = s.current().start;
                self.snippet = Some(s);
                at
            }
            None => Session::final_pos(snip, base),
        };
        let last = self.buf().line_count() - 1;
        let row = at.0.min(last);
        let col = at.1.min(self.line_len(row));
        self.docs[self.cur].cursor = (row, col);
        self.update_want();
    }

    /// The highlighted placeholder, if any: (row, start col, end col).
    pub fn snippet_selection(&self) -> Option<(usize, usize, usize)> {
        let s = self.snippet.as_ref()?;
        if !s.sel || self.mode != Mode::Insert {
            return None;
        }
        let c = s.current();
        Some((c.start.0, c.start.1, c.end.1))
    }

    /// Insert-mode keys that belong to the snippet session. Returns true when the key was consumed.
    pub fn snippet_key(&mut self, k: &Key) -> bool {
        let Some(s) = self.snippet.as_ref() else { return false };
        if self.completion.is_some() {
            return false; // the completion popup owns Tab / Shift-Tab / Enter while it is open
        }
        let sel = s.sel;
        match k {
            Key::Tab => {
                self.snippet_jump(1);
                true
            }
            Key::BackTab => {
                self.snippet_jump(-1);
                true
            }
            Key::Esc
            | Key::Ctrl('c')
            | Key::Ctrl('t')
            | Key::Ctrl('d')
            | Key::Left
            | Key::Right
            | Key::Up
            | Key::Down
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown => {
                self.snippet = None;
                false
            }
            // typing over the selected placeholder replaces it
            Key::Char(_) | Key::Paste(_) | Key::Enter if sel => {
                self.snippet_delete_selection();
                false
            }
            Key::Backspace | Key::Delete | Key::Ctrl('h') | Key::Ctrl('w') | Key::Ctrl('u') if sel => {
                self.snippet_delete_selection();
                true
            }
            _ => false,
        }
    }

    /// Remove the selected placeholder so the next key starts from an empty one.
    fn snippet_delete_selection(&mut self) {
        let Some(s) = self.snippet.as_mut() else { return };
        let cur = s.current().clone();
        s.sel = false;
        let (r, a, b) = (cur.start.0, cur.start.1, cur.end.1);
        if b > a {
            self.buf_mut().delete_range((r, a), (r, b));
            if let Some(s) = self.snippet.as_mut() {
                s.delete_in_row(r, a, b);
            }
        }
        self.docs[self.cur].cursor = cur.start;
        self.update_want();
    }

    /// Move to the next (`dir` > 0) or previous placeholder.
    fn snippet_jump(&mut self, dir: i32) {
        let Some(s) = self.snippet.as_mut() else { return };
        let next = s.cur as i32 + dir;
        let target = if next < 0 {
            // already at the first placeholder: select it again
            s.sel = s.current().end > s.current().start;
            s.current().start
        } else if next as usize >= s.order.len() {
            // past the last one: leave the snippet with the cursor after that placeholder
            let end = s.current().end;
            self.snippet = None;
            end
        } else {
            s.cur = next as usize;
            let stop = s.current().clone();
            s.sel = stop.end > stop.start;
            if s.cur + 1 == s.order.len() && !s.sel {
                self.snippet = None; // the final position: nothing is left to visit
            }
            stop.start
        };
        let last = self.buf().line_count() - 1;
        let row = target.0.min(last);
        let col = target.1.min(self.line_len(row));
        self.docs[self.cur].cursor = (row, col);
        self.update_want();
    }

    /// Taken before an insert-mode key, handed to `snippet_track` afterwards.
    pub fn snippet_snap(&self) -> Option<Snap> {
        let s = self.snippet.as_ref()?;
        let (r, _) = self.cursor();
        Some(Snap { id: s.id, cursor: self.cursor(), nlines: self.buf().line_count(), len: self.line_len(r) })
    }

    /// After a key: shift the placeholders by whatever that key inserted or deleted.
    pub fn snippet_track(&mut self, snap: Option<Snap>) {
        let Some(snap) = snap else { return };
        if self.snippet.as_ref().map(|s| s.id) != Some(snap.id) {
            return; // the key ended the session, or started another one
        }
        if self.mode != Mode::Insert {
            self.snippet = None;
            return;
        }
        let (r, c) = self.cursor();
        let n = self.buf().line_count();
        let len = self.line_len(r);
        let Some(s) = self.snippet.as_mut() else { return };
        if n == snap.nlines && r == snap.cursor.0 {
            if len > snap.len {
                s.insert_at(snap.cursor, len - snap.len);
            } else if len < snap.len {
                let d = snap.len - len;
                if c < snap.cursor.1 {
                    s.delete_in_row(r, snap.cursor.1.saturating_sub(d), snap.cursor.1);
                } else {
                    s.delete_in_row(r, snap.cursor.1, snap.cursor.1 + d);
                }
            }
            // the cursor has to stay inside the placeholder being edited
            let cur = s.current();
            if !(cur.start.0 == r && c >= cur.start.1 && c <= cur.end.1) {
                self.snippet = None;
            }
        } else if n == snap.nlines + 1 && r == snap.cursor.0 + 1 {
            s.split_row(snap.cursor, c);
            s.sel = false;
        } else if n + 1 == snap.nlines && r + 1 == snap.cursor.0 {
            s.join_row(snap.cursor.0, c);
        } else {
            self.snippet = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stops(s: &Snip) -> Vec<(usize, usize, usize)> {
        s.stops.iter().map(|x| (x.index, x.start, x.len)).collect()
    }

    #[test]
    fn plain_text_has_no_stops() {
        let s = parse_snippet("println");
        assert_eq!(s.text, "println");
        assert!(s.stops.is_empty());
    }

    #[test]
    fn placeholders_with_defaults() {
        let s = parse_snippet("foo(${1:first}, ${2:second})$0");
        assert_eq!(s.text, "foo(first, second)");
        assert_eq!(stops(&s), vec![(1, 4, 5), (2, 11, 6), (0, 18, 0)]);
    }

    #[test]
    fn include_style_snippets() {
        let s = parse_snippet("include <${1:header}>");
        assert_eq!(s.text, "include <header>");
        assert_eq!(stops(&s), vec![(1, 9, 6)]);
        let s = parse_snippet("include \"$1\"");
        assert_eq!(s.text, "include \"\"");
        assert_eq!(stops(&s), vec![(1, 9, 0)]);
    }

    #[test]
    fn escapes_choices_nesting_and_variables() {
        assert_eq!(parse_snippet("cost \\$5 \\} \\\\").text, "cost $5 } \\");
        let s = parse_snippet("${1|alpha,beta,gamma|} x");
        assert_eq!(s.text, "alpha x");
        assert_eq!(stops(&s), vec![(1, 0, 5)]);
        let s = parse_snippet("f(${1:g(${2:inner})})");
        assert_eq!(s.text, "f(g(inner))");
        assert_eq!(stops(&s), vec![(1, 2, 8)]); // nested stops are flattened into their parent
        let s = parse_snippet("${TM_FILENAME:file}.$name${NAME}$");
        assert_eq!(s.text, "file.$");
        assert!(s.stops.is_empty());
    }

    #[test]
    fn mirrors_repeat_the_first_text() {
        let s = parse_snippet("${1:x} = $1; ${1:y}");
        assert_eq!(s.text, "x = x; y");
        assert_eq!(stops(&s), vec![(1, 0, 1)]);
    }

    #[test]
    fn broken_snippets_do_not_panic() {
        for src in ["${", "${1", "${1:", "${1:abc", "${1|a,b", "$", "\\", "${:}", "$1$", "}{", "${1:${2:${3:"] {
            let _ = parse_snippet(src);
        }
        assert_eq!(parse_snippet("a}b").text, "a}b");
    }

    #[test]
    fn visiting_order_ends_at_zero() {
        let s = parse_snippet("${2:b} ${1:a} $0");
        let sess = Session::new(&s, (3, 10)).unwrap();
        assert_eq!(sess.current().start, (3, 12)); // `a` comes first although it is written second
        let idx: Vec<usize> = sess.order.iter().map(|i| sess.stops[*i].index).collect();
        assert_eq!(idx, vec![1, 2, 0]);
        // without `$0` the end of the text is the last stop
        let sess = Session::new(&parse_snippet("f(${1:a})"), (0, 0)).unwrap();
        assert_eq!(sess.stops.last().unwrap().start, (0, 4));
        // only a final position: no session at all
        assert!(Session::new(&parse_snippet("f()$0"), (0, 0)).is_none());
        assert!(Session::new(&parse_snippet("f()"), (0, 0)).is_none());
        assert_eq!(Session::final_pos(&parse_snippet("f($0)"), (2, 5)), (2, 7));
        assert_eq!(Session::final_pos(&parse_snippet("f()"), (2, 5)), (2, 8));
    }

    #[test]
    fn multi_line_snippets_get_rows_and_columns() {
        let s = parse_snippet("if (${1:cond}) {\n\t$0\n}");
        let sess = Session::new(&s, (4, 8)).unwrap();
        assert_eq!(sess.current().start, (4, 12));
        let fin = sess.stops.iter().find(|s| s.index == 0).unwrap();
        assert_eq!(fin.start, (5, 1));
    }

    #[test]
    fn indentation_follows_the_line() {
        assert_eq!(adjust_indent("one line", "    ", "  "), "one line");
        assert_eq!(adjust_indent("if (x) {\n\t$0\n}", "    ", "    "), "if (x) {\n        $0\n    }");
        assert_eq!(adjust_indent("a\n\nb\r\n\t\tc", "  ", "\t"), "a\n\n  b\n  \t\tc");
    }

    #[test]
    fn tracking_typing_deleting_and_lines() {
        let s = parse_snippet("f(${1:aa}, ${2:bb})$0");
        let mut sess = Session::new(&s, (0, 0)).unwrap();
        assert_eq!(sess.stops[0], Stop { index: 1, start: (0, 2), end: (0, 4) });
        // the selected placeholder is replaced by three typed chars
        sess.delete_in_row(0, 2, 4);
        assert_eq!(sess.stops[0].end, (0, 2));
        sess.insert_at((0, 2), 3);
        assert_eq!(sess.stops[0], Stop { index: 1, start: (0, 2), end: (0, 5) });
        assert_eq!(sess.stops[1].start, (0, 7));
        assert_eq!(sess.stops[2].start, (0, 10));
        // backspace over one char
        sess.delete_in_row(0, 4, 5);
        assert_eq!(sess.stops[0].end, (0, 4));
        assert_eq!(sess.stops[1].start, (0, 6));
        // Enter inside the first placeholder: what follows moves to the next row
        sess.split_row((0, 3), 4);
        assert_eq!(sess.stops[0], Stop { index: 1, start: (0, 2), end: (0, 3) });
        assert_eq!(sess.stops[1].start, (1, 7));
        // joining the rows again
        sess.join_row(1, 3);
        assert_eq!(sess.stops[1].start, (0, 10));
    }

    #[test]
    fn adjacent_stops_do_not_swallow_each_others_typing() {
        let s = parse_snippet("${1:a}${2:b}$3");
        let mut sess = Session::new(&s, (0, 0)).unwrap();
        // type one char at the end of the first placeholder
        sess.insert_at((0, 1), 1);
        assert_eq!(sess.stops[0].end, (0, 2));
        assert_eq!(sess.stops[1], Stop { index: 2, start: (0, 2), end: (0, 3) });
        assert_eq!(sess.stops[2].start, (0, 3));
        // Enter at the end of the first placeholder leaves it where it was
        sess.split_row((0, 2), 0);
        assert_eq!(sess.stops[0], Stop { index: 1, start: (0, 0), end: (0, 2) });
        assert_eq!(sess.stops[1], Stop { index: 2, start: (1, 0), end: (1, 1) });
    }
}
