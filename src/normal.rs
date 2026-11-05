//! Normal and visual mode: key-sequence parsing, motions, operators, text objects.
//!
//! Keys accumulate in `Editor::keybuf`; after every key the buffer is parsed:
//! `Need` (wait for more), `Bad` (drop it) or `Done` (run it).

use crate::buffer::Pos;
use crate::editor::{idx_at_display, Editor, Mode, Search};
use crate::input::Key;
use crate::motion::{self, ObjRange};

#[derive(Clone, Debug, PartialEq)]
pub enum Mo {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    FirstNonBlank,
    LineEnd,
    Column,
    Word(bool),
    WordEnd(bool),
    WordBack(bool),
    WordEndBack(bool),
    Find { ch: char, fwd: bool, till: bool },
    RepeatFind(bool),
    ParaFwd,
    ParaBack,
    Match,
    Top,
    Bottom,
    ScreenTop,
    ScreenMid,
    ScreenBot,
    SearchNext(bool),
    Star(bool),
    Mark { c: char, line: bool },
    NextLine,
    PrevLine,
    Underscore,
    LastNonBlank,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Delete,
    Change,
    Yank,
    Indent,
    Outdent,
    Upper,
    Lower,
    Toggle,
}

#[derive(Clone, Debug)]
enum Target {
    Motion(Mo, Option<usize>),
    Obj(char, bool),
    Line,
    Selection,
    SelectionLines,
}

#[derive(Clone, Debug)]
enum Cmd {
    Motion(Mo),
    Op(Op, Target),
    Other(&'static str, Option<char>),
    Obj(char, bool),
    /// operator followed by `/` or `?`: the target is typed on the command line
    OpSearch(Op, bool),
}

struct Parsed {
    reg: Option<char>,
    count: Option<usize>,
    cmd: Cmd,
}

enum P {
    Need,
    Bad,
    Done(Parsed),
}

enum PM {
    Need,
    Bad,
}

#[derive(Clone, Copy, Debug)]
pub struct MoRes {
    pub pos: Pos,
    pub linewise: bool,
    pub inclusive: bool,
    pub jump: bool,
    pub vert: bool,
    pub eol: bool,
}

impl MoRes {
    fn new(pos: Pos) -> MoRes {
        MoRes { pos, linewise: false, inclusive: false, jump: false, vert: false, eol: false }
    }
    fn incl(mut self) -> MoRes {
        self.inclusive = true;
        self
    }
    fn lines(mut self) -> MoRes {
        self.linewise = true;
        self
    }
    fn jump(mut self) -> MoRes {
        self.jump = true;
        self
    }
}

#[derive(Clone, Copy, Debug)]
enum Region {
    Chars { start: Pos, end: Pos },
    Lines { r0: usize, r1: usize },
}

const MAX_COUNT: usize = 100_000;

fn parse_count(keys: &[Key], mut i: usize) -> (Option<usize>, usize) {
    let mut n: Option<usize> = None;
    while let Some(Key::Char(c)) = keys.get(i) {
        match c {
            '1'..='9' => n = Some((n.unwrap_or(0) * 10 + (*c as usize - '0' as usize)).min(MAX_COUNT)),
            '0' if n.is_some() => n = Some((n.unwrap() * 10).min(MAX_COUNT)),
            _ => break,
        }
        i += 1;
    }
    (n, i)
}

fn mul_count(a: Option<usize>, b: Option<usize>) -> Option<usize> {
    match (a, b) {
        (None, None) => None,
        (x, y) => Some((x.unwrap_or(1) * y.unwrap_or(1)).min(MAX_COUNT)),
    }
}

fn arg_char(keys: &[Key], at: usize) -> Result<char, PM> {
    match keys.get(at) {
        None => Err(PM::Need),
        Some(Key::Char(c)) => Ok(*c),
        Some(_) => Err(PM::Bad),
    }
}

fn parse_motion(keys: &[Key], i: usize) -> Result<(Mo, usize), PM> {
    let k = keys.get(i).ok_or(PM::Need)?;
    Ok(match k {
        Key::Char('h') | Key::Left | Key::Backspace | Key::Ctrl('h') => (Mo::Left, i + 1),
        Key::Char('l') | Key::Right | Key::Char(' ') => (Mo::Right, i + 1),
        Key::Char('j') | Key::Down | Key::Ctrl('j') => (Mo::Down, i + 1),
        Key::Char('k') | Key::Up => (Mo::Up, i + 1),
        Key::Char('0') | Key::Home => (Mo::LineStart, i + 1),
        Key::Char('^') => (Mo::FirstNonBlank, i + 1),
        Key::Char('$') | Key::End => (Mo::LineEnd, i + 1),
        Key::Char('|') => (Mo::Column, i + 1),
        Key::Char('w') => (Mo::Word(false), i + 1),
        Key::Char('W') => (Mo::Word(true), i + 1),
        Key::Char('e') => (Mo::WordEnd(false), i + 1),
        Key::Char('E') => (Mo::WordEnd(true), i + 1),
        Key::Char('b') => (Mo::WordBack(false), i + 1),
        Key::Char('B') => (Mo::WordBack(true), i + 1),
        Key::Char(c @ ('f' | 'F' | 't' | 'T')) => {
            let ch = arg_char(keys, i + 1)?;
            (Mo::Find { ch, fwd: *c == 'f' || *c == 't', till: *c == 't' || *c == 'T' }, i + 2)
        }
        Key::Char(';') => (Mo::RepeatFind(false), i + 1),
        Key::Char(',') => (Mo::RepeatFind(true), i + 1),
        Key::Char('}') => (Mo::ParaFwd, i + 1),
        Key::Char('{') => (Mo::ParaBack, i + 1),
        Key::Char('%') => (Mo::Match, i + 1),
        Key::Char('G') => (Mo::Bottom, i + 1),
        Key::Char('H') => (Mo::ScreenTop, i + 1),
        Key::Char('M') => (Mo::ScreenMid, i + 1),
        Key::Char('L') => (Mo::ScreenBot, i + 1),
        Key::Char('n') => (Mo::SearchNext(false), i + 1),
        Key::Char('N') => (Mo::SearchNext(true), i + 1),
        Key::Char('*') => (Mo::Star(true), i + 1),
        Key::Char('#') => (Mo::Star(false), i + 1),
        Key::Char('+') | Key::Enter => (Mo::NextLine, i + 1),
        Key::Char('-') => (Mo::PrevLine, i + 1),
        Key::Char('_') => (Mo::Underscore, i + 1),
        Key::Char('\'') => (Mo::Mark { c: arg_char(keys, i + 1)?, line: true }, i + 2),
        Key::Char('`') => (Mo::Mark { c: arg_char(keys, i + 1)?, line: false }, i + 2),
        Key::Char('g') => match keys.get(i + 1) {
            None => return Err(PM::Need),
            Some(Key::Char('g')) => (Mo::Top, i + 2),
            Some(Key::Char('e')) => (Mo::WordEndBack(false), i + 2),
            Some(Key::Char('E')) => (Mo::WordEndBack(true), i + 2),
            Some(Key::Char('_')) => (Mo::LastNonBlank, i + 2),
            Some(Key::Char('j')) => (Mo::Down, i + 2),
            Some(Key::Char('k')) => (Mo::Up, i + 2),
            Some(_) => return Err(PM::Bad),
        },
        _ => return Err(PM::Bad),
    })
}

fn op_from_key(k: &Key) -> Option<(Op, char)> {
    match k {
        Key::Char('d') => Some((Op::Delete, 'd')),
        Key::Char('c') => Some((Op::Change, 'c')),
        Key::Char('y') => Some((Op::Yank, 'y')),
        Key::Char('>') => Some((Op::Indent, '>')),
        Key::Char('<') => Some((Op::Outdent, '<')),
        _ => None,
    }
}

fn parse_op_target(keys: &[Key], i: usize, op_last: char, count1: Option<usize>, op: Op, reg: Option<char>) -> P {
    let (count2, j) = parse_count(keys, i);
    let k = match keys.get(j) {
        None => return P::Need,
        Some(k) => k,
    };
    let done = |t: Target, count: Option<usize>| P::Done(Parsed { reg, count, cmd: Cmd::Op(op, t) });
    // doubled operator: dd, cc, >>, g~~, guu ...
    if *k == Key::Char(op_last) {
        return done(Target::Line, mul_count(count1, count2));
    }
    if *k == Key::Char('g') && matches!(op, Op::Toggle | Op::Upper | Op::Lower) {
        match keys.get(j + 1) {
            None => return P::Need,
            Some(Key::Char(c)) if *c == 'g' => {
                // gUgg is a motion, not a doubled operator -- fall through to the motion parser
            }
            Some(Key::Char(c)) if *c == op_last => return done(Target::Line, mul_count(count1, count2)),
            _ => {}
        }
    }
    if let Key::Char(c @ ('/' | '?')) = k {
        return P::Done(Parsed { reg, count: mul_count(count1, count2), cmd: Cmd::OpSearch(op, *c == '/') });
    }
    if let Key::Char(c @ ('i' | 'a')) = k {
        return match keys.get(j + 1) {
            None => P::Need,
            Some(Key::Char(o)) => done(Target::Obj(*o, *c == 'a'), mul_count(count1, count2)),
            Some(_) => P::Bad,
        };
    }
    match parse_motion(keys, j) {
        Ok((mo, _)) => done(Target::Motion(mo, count2), count1),
        Err(PM::Need) => P::Need,
        Err(PM::Bad) => P::Bad,
    }
}

fn parse(keys: &[Key], visual: bool) -> P {
    let mut i = 0;
    let mut reg = None;
    if keys.first() == Some(&Key::Char('"')) {
        match keys.get(1) {
            None => return P::Need,
            Some(Key::Char(c)) => {
                reg = Some(*c);
                i = 2;
            }
            Some(_) => return P::Bad,
        }
    }
    let (count, ni) = parse_count(keys, i);
    i = ni;
    let k = match keys.get(i) {
        None => return P::Need,
        Some(k) => k.clone(),
    };
    let other = |name: &'static str, arg: Option<char>, adv: usize| -> P {
        let _ = adv;
        P::Done(Parsed { reg, count, cmd: Cmd::Other(name, arg) })
    };

    if visual {
        // operators act on the selection immediately
        let sel = |op: Op, t: Target| P::Done(Parsed { reg, count, cmd: Cmd::Op(op, t) });
        match &k {
            Key::Char('d') | Key::Char('x') | Key::Delete => return sel(Op::Delete, Target::Selection),
            Key::Char('c') | Key::Char('s') => return sel(Op::Change, Target::Selection),
            Key::Char('y') => return sel(Op::Yank, Target::Selection),
            Key::Char('>') => return sel(Op::Indent, Target::Selection),
            Key::Char('<') => return sel(Op::Outdent, Target::Selection),
            Key::Char('~') => return sel(Op::Toggle, Target::Selection),
            Key::Char('u') => return sel(Op::Lower, Target::Selection),
            Key::Char('U') => return sel(Op::Upper, Target::Selection),
            Key::Char('D') | Key::Char('X') => return sel(Op::Delete, Target::SelectionLines),
            Key::Char('Y') => return sel(Op::Yank, Target::SelectionLines),
            Key::Char('C') | Key::Char('S') | Key::Char('R') => return sel(Op::Change, Target::SelectionLines),
            Key::Char(c @ ('i' | 'a')) => {
                return match keys.get(i + 1) {
                    None => P::Need,
                    Some(Key::Char(o)) => P::Done(Parsed { reg, count, cmd: Cmd::Obj(*o, *c == 'a') }),
                    Some(_) => P::Bad,
                };
            }
            Key::Char('o') | Key::Char('O') => return other("v_o", None, 1),
            Key::Char('J') => return other("J", None, 1),
            Key::Char('p') | Key::Char('P') => return other("v_p", None, 1),
            Key::Char('r') => {
                return match keys.get(i + 1) {
                    None => P::Need,
                    Some(Key::Char(c)) => other("v_r", Some(*c), 2),
                    Some(_) => P::Bad,
                };
            }
            Key::Char('v') => return other("v", None, 1),
            Key::Char('V') => return other("V", None, 1),
            Key::Ctrl('v') => return other("Ctrl-v", None, 1),
            Key::Char(':') => return other("colon", None, 1),
            Key::Char('g')
                if keys.get(i + 1) == Some(&Key::Char('v')) => {
                    return other("gv", None, 2);
                }
            _ => {}
        }
    } else {
        if let Some((op, last)) = op_from_key(&k) {
            return parse_op_target(keys, i + 1, last, count, op, reg);
        }
        if k == Key::Char('g') {
            let g2 = match keys.get(i + 1) {
                None => return P::Need,
                Some(k) => k.clone(),
            };
            let gop = match g2 {
                Key::Char('~') => Some((Op::Toggle, '~')),
                Key::Char('u') => Some((Op::Lower, 'u')),
                Key::Char('U') => Some((Op::Upper, 'U')),
                _ => None,
            };
            if let Some((op, last)) = gop {
                return parse_op_target(keys, i + 2, last, count, op, reg);
            }
        }
    }

    // non-operator commands
    let simple = |name: &'static str| other(name, None, 1);
    match &k {
        Key::Char('x') | Key::Delete => return simple("x"),
        Key::Char('X') => return simple("X"),
        Key::Char('s') => return simple("s"),
        Key::Char('S') => return simple("S"),
        Key::Char('C') => return simple("C"),
        Key::Char('D') => return simple("D"),
        Key::Char('Y') => return simple("Y"),
        Key::Char('p') => return simple("p"),
        Key::Char('P') => return simple("P"),
        Key::Char('u') => return simple("u"),
        Key::Ctrl('r') => return simple("redo"),
        Key::Char('J') => return simple("J"),
        Key::Char('~') => return simple("~"),
        Key::Char('.') => return simple("."),
        Key::Char('i') | Key::Insert => return simple("i"),
        Key::Char('a') => return simple("a"),
        Key::Char('I') => return simple("I"),
        Key::Char('A') => return simple("A"),
        Key::Char('o') => return simple("o"),
        Key::Char('O') => return simple("O"),
        Key::Char('v') => return simple("v"),
        Key::Char('V') => return simple("V"),
        Key::Ctrl('v') => return simple("Ctrl-v"),
        Key::Char(':') => return simple("colon"),
        Key::Char('/') => return simple("slash"),
        Key::Char('?') => return simple("question"),
        Key::Char('K') => return simple("K"),
        Key::Char('&') => return simple("&"),
        Key::Ctrl('d') => return simple("Ctrl-d"),
        Key::Ctrl('u') => return simple("Ctrl-u"),
        Key::Ctrl('f') | Key::PageDown => return simple("Ctrl-f"),
        Key::Ctrl('b') | Key::PageUp => return simple("Ctrl-b"),
        Key::Ctrl('e') => return simple("Ctrl-e"),
        Key::Ctrl('y') => return simple("Ctrl-y"),
        Key::Ctrl('o') => return simple("Ctrl-o"),
        Key::Tab => return simple("Tab"),
        Key::Ctrl('a') => return simple("Ctrl-a"),
        Key::Ctrl('x') => return simple("Ctrl-x"),
        Key::Ctrl('p') => return simple("files"),
        Key::Ctrl(']') => return simple("gd"),
        Key::Char('r') => {
            return match keys.get(i + 1) {
                None => P::Need,
                Some(Key::Char(c)) => other("r", Some(*c), 2),
                Some(Key::Enter) => other("r", Some('\n'), 2),
                Some(_) => P::Bad,
            };
        }
        Key::Char('q') => {
            return match keys.get(i + 1) {
                None => P::Need,
                Some(Key::Char(c)) => other("q", Some(*c), 2),
                Some(_) => P::Bad,
            };
        }
        Key::Char('@') => {
            return match keys.get(i + 1) {
                None => P::Need,
                Some(Key::Char(c)) => other("@", Some(*c), 2),
                Some(_) => P::Bad,
            };
        }
        Key::Char('m') => {
            return match keys.get(i + 1) {
                None => P::Need,
                Some(Key::Char(c)) => other("m", Some(*c), 2),
                Some(_) => P::Bad,
            };
        }
        Key::Char('Z') => {
            return match keys.get(i + 1) {
                None => P::Need,
                Some(Key::Char('Z')) => simple("ZZ"),
                Some(Key::Char('Q')) => simple("ZQ"),
                Some(_) => P::Bad,
            };
        }
        Key::Char('z') => {
            return match keys.get(i + 1) {
                None => P::Need,
                Some(Key::Char('z')) | Some(Key::Char('.')) => simple("zz"),
                Some(Key::Char('t')) | Some(Key::Enter) => simple("zt"),
                Some(Key::Char('b')) | Some(Key::Char('-')) => simple("zb"),
                Some(_) => P::Bad,
            };
        }
        Key::Char(c @ ('[' | ']')) => {
            return match keys.get(i + 1) {
                None => P::Need,
                Some(Key::Char('d')) => simple(if *c == ']' { "]d" } else { "[d" }),
                Some(_) => P::Bad,
            };
        }
        Key::Ctrl('w') => {
            return match keys.get(i + 1) {
                None => P::Need,
                Some(Key::Char(c)) => other("ctrl-w", Some(*c), 2),
                Some(Key::Ctrl(c)) => other("ctrl-w", Some(*c), 2),
                Some(_) => P::Bad,
            };
        }
        Key::Paste(_) => return simple("paste"),
        Key::Char('g') => match keys.get(i + 1) {
            None => return P::Need,
            Some(Key::Char('d')) => return simple("gd"),
            Some(Key::Char('r')) => return simple("gr"),
            Some(Key::Char('i')) => return simple("gi"),
            Some(Key::Char('I')) => return simple("gI"),
            Some(Key::Char('J')) => return simple("gJ"),
            Some(Key::Char('v')) => return simple("gv"),
            Some(Key::Char('p')) => return simple("gp"),
            Some(Key::Char('P')) => return simple("gP"),
            _ => {}
        },
        _ => {}
    }
    match parse_motion(keys, i) {
        Ok((mo, _)) => P::Done(Parsed { reg, count, cmd: Cmd::Motion(mo) }),
        Err(PM::Need) => P::Need,
        Err(PM::Bad) => P::Bad,
    }
}

fn map_case(op: Op, c: char) -> String {
    match op {
        Op::Upper => c.to_uppercase().collect(),
        Op::Lower => c.to_lowercase().collect(),
        _ => {
            if c.is_uppercase() {
                c.to_lowercase().collect()
            } else if c.is_lowercase() {
                c.to_uppercase().collect()
            } else {
                c.to_string()
            }
        }
    }
}

fn order(a: Pos, b: Pos) -> (Pos, Pos) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

impl Editor {
    pub fn normal_key(&mut self, k: Key) {
        let visual = self.mode != Mode::Normal;
        if matches!(k, Key::Esc | Key::Ctrl('c')) {
            self.keybuf.clear();
            if visual {
                self.leave_visual();
            }
            return;
        }
        if k == Key::Char('q') && self.keybuf.is_empty() && self.recording.is_some() {
            // stop recording; the `q` itself was already appended to macro_buf
            let mut keys = std::mem::take(&mut self.macro_buf);
            keys.pop();
            if let Some(name) = self.recording.take() {
                self.macros.insert(name, keys);
            }
            self.msg.clear();
            return;
        }
        if self.keybuf.is_empty() && !self.in_dot {
            self.cur_change.clear();
        }
        if !self.in_dot {
            self.cur_change.push(k.clone());
        }
        self.keybuf.push(k);
        match parse(&self.keybuf, visual) {
            P::Need => {}
            P::Bad => self.keybuf.clear(),
            P::Done(p) => {
                self.keybuf.clear();
                self.run(p, visual);
            }
        }
    }

    fn run(&mut self, p: Parsed, visual: bool) {
        let v0 = self.buf().version;
        let doc = self.cur;
        let c = self.cursor();
        self.buf_mut().begin_group(c);
        self.no_dot = false;
        self.exec(p);
        if self.mode != Mode::Insert {
            self.finish_group();
        }
        if !visual && !self.no_dot && !self.in_dot && self.mode != Mode::Insert && self.cur == doc && self.buf().version != v0 {
            self.last_change = std::mem::take(&mut self.cur_change);
        }
        self.clamp_cursor();
    }

    fn exec(&mut self, p: Parsed) {
        let Parsed { reg, count, cmd } = p;
        match cmd {
            Cmd::Motion(mo) => self.do_motion(&mo, count),
            Cmd::Op(op, target) => self.do_op(op, reg, count, target),
            Cmd::Obj(ch, around) => self.select_object(ch, around, count),
            Cmd::Other(name, arg) => self.do_other(name, arg, reg, count),
            Cmd::OpSearch(op, fwd) => {
                self.search_op = Some((op, reg, count));
                self.start_cmdline(if fwd { '/' } else { '?' }, "");
            }
        }
    }

    // ---- visual mode helpers --------------------------------------------

    pub fn enter_visual(&mut self, mode: Mode) {
        self.visual_anchor = self.cursor();
        self.mode = mode;
    }

    pub fn leave_visual(&mut self) {
        if self.mode == Mode::Visual || self.mode == Mode::VisualLine {
            let a = self.visual_anchor;
            let b = self.cursor();
            self.last_visual = Some((self.mode, a, b));
            let (s, e) = order(a, b);
            let d = self.doc_mut();
            d.marks.insert('<', s);
            d.marks.insert('>', e);
        }
        self.mode = Mode::Normal;
        self.clamp_cursor();
    }

    fn visual_region(&self, force_lines: bool) -> Region {
        let (s, e) = order(self.visual_anchor, self.cursor());
        if force_lines || self.mode == Mode::VisualLine {
            return Region::Lines { r0: s.0, r1: e.0 };
        }
        let len = self.line_len(e.0);
        let end = if e.1 >= len {
            if e.0 + 1 < self.buf().line_count() {
                (e.0 + 1, 0)
            } else {
                (e.0, len)
            }
        } else {
            (e.0, e.1 + 1)
        };
        Region::Chars { start: s, end }
    }

    // ---- motions --------------------------------------------------------

    fn do_motion(&mut self, mo: &Mo, count: Option<usize>) {
        let res = self.eval_motion(mo, count, false);
        let Some(r) = res else { return };
        if r.jump {
            self.push_jump();
        }
        self.docs[self.cur].cursor = r.pos;
        self.clamp_cursor();
        if r.eol {
            self.docs[self.cur].want = usize::MAX / 2;
        } else if !r.vert {
            self.update_want();
        }
    }

    pub fn eval_motion(&mut self, mo: &Mo, count: Option<usize>, opp: bool) -> Option<MoRes> {
        let n = count.unwrap_or(1).max(1);
        let (r, c) = self.cursor();
        let last = self.buf().line_count() - 1;
        let ts = self.opts.tabstop;
        let len = self.line_len(r);
        match mo {
            Mo::Left => {
                if c == 0 {
                    return None;
                }
                Some(MoRes::new((r, c.saturating_sub(n))))
            }
            Mo::Right => {
                let max = if opp { len } else { len.saturating_sub(1) };
                let t = (c + n).min(max);
                if t == c && !opp {
                    return None;
                }
                Some(MoRes::new((r, t)))
            }
            Mo::Up | Mo::Down => {
                let nr = if *mo == Mo::Up {
                    if r == 0 {
                        return None;
                    }
                    r.saturating_sub(n)
                } else {
                    if r >= last {
                        return None;
                    }
                    (r + n).min(last)
                };
                let want = self.doc().want;
                let line = &self.lines()[nr];
                let max = line.len().saturating_sub(if opp { 0 } else { 1 });
                let col = idx_at_display(line, want, ts).min(max);
                let mut m = MoRes::new((nr, col)).lines();
                m.vert = true;
                Some(m)
            }
            Mo::LineStart => Some(MoRes::new((r, 0))),
            Mo::FirstNonBlank => Some(MoRes::new((r, self.first_nonblank(r)))),
            Mo::LineEnd => {
                let nr = (r + n - 1).min(last);
                let l = self.line_len(nr);
                let mut m = MoRes::new((nr, l.saturating_sub(1))).incl();
                m.eol = true;
                Some(m)
            }
            Mo::Column => {
                let line = &self.lines()[r];
                let col = idx_at_display(line, n - 1, ts).min(line.len().saturating_sub(1));
                Some(MoRes::new((r, col)))
            }
            Mo::Word(big) => {
                let mut p = (r, c);
                for _ in 0..n {
                    p = motion::word_fwd(self.lines(), p, *big);
                }
                if p == (r, c) {
                    return None;
                }
                Some(MoRes::new(p))
            }
            Mo::WordEnd(big) => {
                let mut p = (r, c);
                for _ in 0..n {
                    p = motion::word_end(self.lines(), p, *big);
                }
                Some(MoRes::new(p).incl())
            }
            Mo::WordBack(big) => {
                let mut p = (r, c);
                for _ in 0..n {
                    p = motion::word_back(self.lines(), p, *big);
                }
                if p == (r, c) {
                    return None;
                }
                Some(MoRes::new(p))
            }
            Mo::WordEndBack(big) => {
                let mut p = (r, c);
                for _ in 0..n {
                    p = motion::word_end_back(self.lines(), p, *big);
                }
                Some(MoRes::new(p).incl())
            }
            Mo::Find { ch, fwd, till } => {
                let t = motion::find_char(&self.lines()[r], c, *ch, *fwd, *till, n, false)?;
                self.last_find = Some((*ch, *fwd, *till));
                let m = MoRes::new((r, t));
                Some(if *fwd { m.incl() } else { m })
            }
            Mo::RepeatFind(rev) => {
                let (ch, fwd, till) = self.last_find?;
                let fwd = fwd != *rev;
                let t = motion::find_char(&self.lines()[r], c, ch, fwd, till, n, true)?;
                let m = MoRes::new((r, t));
                Some(if fwd { m.incl() } else { m })
            }
            Mo::ParaFwd => {
                let mut p = (r, c);
                for _ in 0..n {
                    p = motion::para_fwd(self.lines(), p.0);
                }
                Some(MoRes::new(p).jump())
            }
            Mo::ParaBack => {
                let mut p = (r, c);
                for _ in 0..n {
                    p = motion::para_back(self.lines(), p.0);
                }
                Some(MoRes::new(p).jump())
            }
            Mo::Match => {
                let p = motion::match_pair(self.lines(), (r, c))?;
                Some(MoRes::new(p).incl().jump())
            }
            Mo::Top | Mo::Bottom => {
                let row = match (mo, count) {
                    (Mo::Top, None) => 0,
                    (Mo::Bottom, None) => last,
                    (_, Some(k)) => (k - 1).min(last),
                    _ => 0,
                };
                Some(MoRes::new((row, self.first_nonblank(row))).lines().jump())
            }
            Mo::ScreenTop | Mo::ScreenMid | Mo::ScreenBot => {
                let top = self.doc().top.min(last);
                let bot = (top + self.view_h.saturating_sub(1)).min(last);
                let row = match mo {
                    Mo::ScreenTop => (top + n - 1).min(bot),
                    Mo::ScreenBot => bot.saturating_sub(n - 1).max(top),
                    _ => (top + bot) / 2,
                };
                Some(MoRes::new((row, self.first_nonblank(row))).lines().jump())
            }
            Mo::SearchNext(rev) => {
                let s = match self.last_search.clone() {
                    Some(s) => s,
                    None => {
                        self.set_err("No previous search pattern");
                        return None;
                    }
                };
                self.hl_search = true;
                self.search_target(&s, s.forward != *rev, n)
            }
            Mo::Star(fwd) => {
                let line = self.lines()[r].clone();
                let mut s = c.min(line.len().saturating_sub(1));
                // on a non-word char: use the next word on the line
                while s < line.len() && !motion::is_word_char(line[s]) {
                    s += 1;
                }
                if s >= line.len() {
                    self.set_err("No string under cursor");
                    return None;
                }
                let mut b = s;
                while b > 0 && motion::is_word_char(line[b - 1]) {
                    b -= 1;
                }
                let mut e = s;
                while e < line.len() && motion::is_word_char(line[e]) {
                    e += 1;
                }
                let word: Vec<char> = line[b..e].to_vec();
                let sr = Search { pat: word, forward: *fwd, whole: true };
                self.last_search = Some(sr.clone());
                self.hl_search = true;
                // search from the start of the word so `#` doesn't find itself
                let from = (r, b);
                let t = self.find_n(from, &sr, *fwd, n)?;
                Some(MoRes::new(t).jump())
            }
            Mo::Mark { c: mc, line } => {
                let p = self.mark_pos(*mc)?;
                let p = self.buf().clamp(p);
                let jump = true;
                if *line {
                    Some(MoRes { pos: (p.0, self.first_nonblank(p.0)), linewise: true, inclusive: false, jump, vert: false, eol: false })
                } else {
                    Some(MoRes::new(p).jump())
                }
            }
            Mo::NextLine | Mo::PrevLine | Mo::Underscore => {
                let row = match mo {
                    Mo::NextLine => {
                        if r >= last {
                            return None;
                        }
                        (r + n).min(last)
                    }
                    Mo::PrevLine => {
                        if r == 0 {
                            return None;
                        }
                        r.saturating_sub(n)
                    }
                    _ => (r + n - 1).min(last),
                };
                Some(MoRes::new((row, self.first_nonblank(row))).lines())
            }
            Mo::LastNonBlank => {
                let nr = (r + n - 1).min(last);
                let l = &self.lines()[nr];
                let mut e = l.len();
                while e > 0 && l[e - 1].is_whitespace() {
                    e -= 1;
                }
                Some(MoRes::new((nr, e.saturating_sub(1))).incl())
            }
        }
    }

    fn find_n(&mut self, from: Pos, s: &Search, forward: bool, n: usize) -> Option<Pos> {
        let mut pos = from;
        for _ in 0..n.max(1) {
            if self.timed_out() {
                return None;
            }
            match self.find_match(pos, s, forward) {
                Some((p, _)) => pos = p,
                None => {
                    let p: String = s.pat.iter().collect();
                    self.set_err(format!("Pattern not found: {}", p));
                    return None;
                }
            }
        }
        Some(pos)
    }

    fn search_target(&mut self, s: &Search, forward: bool, n: usize) -> Option<MoRes> {
        let from = self.cursor();
        let p = self.find_n(from, s, forward, n)?;
        Some(MoRes::new(p).jump())
    }

    fn mark_pos(&mut self, c: char) -> Option<Pos> {
        let p = match c {
            '`' => self.doc().marks.get(&'\'').copied(),
            _ => self.doc().marks.get(&c).copied(),
        };
        if p.is_none() {
            self.set_err("Mark not set");
        }
        p
    }

    // ---- text objects ---------------------------------------------------

    fn eval_object(&self, ch: char, around: bool, count: Option<usize>) -> Option<ObjRange> {
        let p = self.cursor();
        let l = self.lines();
        let n = count.unwrap_or(1).max(1);
        match ch {
            'w' | 'W' => {
                let big = ch == 'W';
                let mut r = motion::obj_word(l, p, around, big)?;
                for _ in 1..n {
                    if r.end.1 >= l[r.end.0].len() {
                        break;
                    }
                    let nx = motion::obj_word(l, r.end, around, big)?;
                    r.end = nx.end;
                }
                Some(r)
            }
            '"' | '\'' | '`' => motion::obj_quote(l, p, ch, around),
            '(' | ')' | 'b' => motion::obj_bracket_n(l, p, '(', ')', around, n),
            '{' | '}' | 'B' => motion::obj_bracket_n(l, p, '{', '}', around, n),
            '[' | ']' => motion::obj_bracket_n(l, p, '[', ']', around, n),
            '<' | '>' => motion::obj_bracket_n(l, p, '<', '>', around, n),
            'p' => motion::obj_paragraph(l, p.0, around),
            _ => None,
        }
    }

    fn select_object(&mut self, ch: char, around: bool, count: Option<usize>) {
        let Some(o) = self.eval_object(ch, around, count) else { return };
        if o.linewise {
            self.visual_anchor = (o.start.0, 0);
            self.docs[self.cur].cursor = (o.end.0, 0);
            self.mode = Mode::VisualLine;
        } else if o.end > o.start {
            self.visual_anchor = o.start;
            let e = if o.end.1 > 0 { (o.end.0, o.end.1 - 1) } else { o.end };
            self.docs[self.cur].cursor = e;
        }
        self.update_want();
    }

    // ---- operators ------------------------------------------------------

    fn do_op(&mut self, op: Op, reg: Option<char>, count: Option<usize>, target: Target) {
        let visual = matches!(target, Target::Selection | Target::SelectionLines);
        let region = match target {
            Target::Selection => Some(self.visual_region(false)),
            Target::SelectionLines => Some(self.visual_region(true)),
            Target::Line => {
                let n = count.unwrap_or(1).max(1);
                let r0 = self.cursor().0;
                let last = self.buf().line_count() - 1;
                Some(Region::Lines { r0, r1: (r0 + n - 1).min(last) })
            }
            Target::Obj(ch, around) => self.eval_object(ch, around, count).map(|o| {
                if o.linewise {
                    Region::Lines { r0: o.start.0, r1: o.end.0 }
                } else {
                    Region::Chars { start: o.start, end: o.end }
                }
            }),
            Target::Motion(mo, c2) => self.motion_region(op, &mo, mul_count(count, c2)),
        };
        // indent operators use the count as a shift amount in visual mode
        let levels = if visual { count.unwrap_or(1) } else { 1 };
        if visual {
            self.leave_visual();
        }
        let Some(region) = region else { return };
        self.apply_op(op, reg, levels, region);
    }

    fn motion_region(&mut self, op: Op, mo: &Mo, count: Option<usize>) -> Option<Region> {
        let start = self.cursor();
        let n = count.unwrap_or(1).max(1);
        // `cw` on a word behaves like `ce`
        if op == Op::Change {
            if let Mo::Word(big) = mo {
                let line = self.lines()[start.0].clone();
                if start.1 < line.len() && !line[start.1].is_whitespace() {
                    let mut p = start;
                    let k = motion::class(line[start.1], *big);
                    let mut i = start.1;
                    while i + 1 < line.len() && motion::class(line[i + 1], *big) == k {
                        i += 1;
                    }
                    p.1 = i;
                    for _ in 1..n {
                        p = motion::word_end(self.lines(), p, *big);
                    }
                    return Some(Region::Chars { start, end: (p.0, p.1 + 1) });
                }
                if n == 1 {
                    // on blanks: change only the blank run
                    let mut i = start.1;
                    while i < line.len() && line[i].is_whitespace() {
                        i += 1;
                    }
                    return Some(Region::Chars { start, end: (start.0, i.max(start.1 + 1).min(line.len())) });
                }
            }
        }
        let mut res = self.eval_motion(mo, count, true)?;
        // `dw` on the last word of a line stops at the end of that line
        if matches!(mo, Mo::Word(_)) && res.pos.0 > start.0 {
            let first = {
                let l = &self.lines()[res.pos.0];
                l.iter().take_while(|c| c.is_whitespace()).count()
            };
            if res.pos.1 <= first {
                let pr = res.pos.0 - 1;
                res.pos = (pr, self.line_len(pr));
            }
        }
        if res.jump {
            self.doc_mut().marks.insert('\'', start);
        }
        Some(self.region_from(start, res))
    }

    /// Turn a motion result into the region an operator acts on (vim's exclusive/inclusive/linewise rules).
    fn region_from(&self, start: Pos, res: MoRes) -> Region {
        let (a, b) = order(start, res.pos);
        if res.linewise {
            return Region::Lines { r0: a.0, r1: b.0 };
        }
        if res.inclusive {
            return Region::Chars { start: a, end: (b.0, b.1 + 1) };
        }
        // exclusive: an end in column 0 moves back to the end of the previous line, and the
        // motion becomes linewise when it also started at or before the first non-blank
        if b.0 > a.0 && b.1 == 0 {
            let pr = b.0 - 1;
            let first = self.first_nonblank_idx(a.0);
            if a.1 <= first {
                return Region::Lines { r0: a.0, r1: pr };
            }
            return Region::Chars { start: a, end: (pr, self.line_len(pr)) };
        }
        Region::Chars { start: a, end: b }
    }

    /// Second half of `d/pattern<CR>`: the pattern has just been typed.
    pub fn finish_search_op(&mut self, sop: (Op, Option<char>, Option<usize>), pat: &str, fwd: bool) {
        let (op, reg, count) = sop;
        if !pat.is_empty() {
            self.last_search = Some(Search { pat: pat.chars().collect(), forward: fwd, whole: false });
        }
        let Some(s) = self.last_search.clone() else {
            self.set_err("No previous search pattern");
            return;
        };
        self.hl_search = true;
        let start = self.cursor();
        let Some(p) = self.find_n(start, &s, fwd, count.unwrap_or(1)) else { return };
        self.buf_mut().begin_group(start);
        self.doc_mut().marks.insert('\'', start);
        let region = self.region_from(start, MoRes::new(p));
        self.apply_op(op, reg, 1, region);
        if self.mode != Mode::Insert {
            self.finish_group();
        }
        self.clamp_cursor();
    }

    /// Index of the first non-blank char (len when the line is blank).
    fn first_nonblank_idx(&self, r: usize) -> usize {
        self.lines()[r].iter().take_while(|c| **c == ' ' || **c == '\t').count()
    }

    fn apply_op(&mut self, op: Op, reg: Option<char>, levels: usize, region: Region) {
        match region {
            Region::Lines { r0, r1 } => {
                let text: Vec<char> = {
                    let mut t = Vec::new();
                    for r in r0..=r1 {
                        if r > r0 {
                            t.push('\n');
                        }
                        t.extend_from_slice(&self.lines()[r]);
                    }
                    t
                };
                match op {
                    Op::Yank => {
                        self.set_reg(reg, text, true, true);
                        let n = r1 - r0 + 1;
                        if n > 2 {
                            self.set_msg(format!("{} lines yanked", n));
                        }
                        let (cr, cc) = self.cursor();
                        let nr = cr.min(r0);
                        self.move_to((nr, if nr == cr { cc } else { 0 }));
                    }
                    Op::Delete => {
                        self.set_reg(reg, text, true, false);
                        self.buf_mut().delete_lines(r0, r1);
                        let row = r0.min(self.buf().line_count() - 1);
                        let col = self.first_nonblank(row);
                        self.move_to((row, col));
                        let n = r1 - r0 + 1;
                        if n > 2 {
                            self.set_msg(format!("{} fewer lines", n));
                        }
                    }
                    Op::Change => {
                        self.set_reg(reg, text, true, false);
                        let indent: Vec<char> = if self.opts.autoindent {
                            let l = &self.lines()[r0];
                            let k = l.iter().take_while(|c| **c == ' ' || **c == '\t').count();
                            l[..k].to_vec()
                        } else {
                            Vec::new()
                        };
                        if r1 > r0 {
                            self.buf_mut().delete_lines(r0 + 1, r1);
                        }
                        let col = indent.len();
                        self.buf_mut().set_line(r0, indent);
                        self.docs[self.cur].cursor = (r0, col);
                        self.enter_insert(1, None);
                    }
                    Op::Indent | Op::Outdent => {
                        let d = if op == Op::Indent { levels as isize } else { -(levels as isize) };
                        for r in r0..=r1 {
                            self.shift_line(r, d);
                        }
                        let col = self.first_nonblank(r0);
                        self.move_to((r0, col));
                        if r1 > r0 + 1 {
                            self.set_msg(format!("{} lines {}ed", r1 - r0 + 1, if op == Op::Indent { ">" } else { "<" }));
                        }
                    }
                    Op::Upper | Op::Lower | Op::Toggle => {
                        for r in r0..=r1 {
                            let nl: Vec<char> = self.lines()[r].iter().flat_map(|c| map_case(op, *c).chars().collect::<Vec<_>>()).collect();
                            self.buf_mut().set_line(r, nl);
                        }
                        self.move_to((r0, 0));
                    }
                }
            }
            Region::Chars { start, end } => {
                let end = self.buf().clamp(end);
                let text = self.buf().get_range(start, end);
                match op {
                    Op::Yank => {
                        let multi = text.contains(&'\n');
                        let n = text.iter().filter(|c| **c == '\n').count() + 1;
                        self.set_reg(reg, text, false, true);
                        if multi && n > 2 {
                            self.set_msg(format!("{} lines yanked", n));
                        }
                        self.move_to(start);
                    }
                    Op::Delete => {
                        if start == end {
                            return;
                        }
                        self.set_reg(reg, text, false, false);
                        self.buf_mut().delete_range(start, end);
                        self.move_to(start);
                    }
                    Op::Change => {
                        self.set_reg(reg, text, false, false);
                        self.buf_mut().delete_range(start, end);
                        self.docs[self.cur].cursor = start;
                        self.enter_insert(1, None);
                    }
                    Op::Indent | Op::Outdent => {
                        let d = if op == Op::Indent { levels as isize } else { -(levels as isize) };
                        for r in start.0..=end.0.max(start.0) {
                            self.shift_line(r, d);
                        }
                        let col = self.first_nonblank(start.0);
                        self.move_to((start.0, col));
                    }
                    Op::Upper | Op::Lower | Op::Toggle => {
                        let nt: Vec<char> = text.iter().flat_map(|c| map_case(op, *c).chars().collect::<Vec<_>>()).collect();
                        self.buf_mut().delete_range(start, end);
                        self.buf_mut().insert_text(start, &nt);
                        self.move_to(start);
                    }
                }
            }
        }
    }

    // ---- everything else ------------------------------------------------

    fn simple_op(&mut self, op: Op, reg: Option<char>, count: Option<usize>, mo: Mo, line_target: bool) {
        let target = if line_target { Target::Line } else { Target::Motion(mo, None) };
        self.do_op(op, reg, count, target);
    }

    fn do_other(&mut self, name: &'static str, arg: Option<char>, reg: Option<char>, count: Option<usize>) {
        let n = count.unwrap_or(1).max(1);
        match name {
            "x" => self.simple_op(Op::Delete, reg, count, Mo::Right, false),
            "X" => self.simple_op(Op::Delete, reg, count, Mo::Left, false),
            "s" => self.simple_op(Op::Change, reg, count, Mo::Right, false),
            "S" => self.simple_op(Op::Change, reg, count, Mo::Right, true),
            "C" => self.simple_op(Op::Change, reg, count, Mo::LineEnd, false),
            "D" => self.simple_op(Op::Delete, reg, count, Mo::LineEnd, false),
            "Y" => self.simple_op(Op::Yank, reg, count, Mo::Right, true),
            "p" | "gp" => self.paste(reg, n, true),
            "P" | "gP" => self.paste(reg, n, false),
            "v_p" => self.visual_paste(reg),
            "u" => {
                self.no_dot = true;
                let mut done = 0;
                for _ in 0..n {
                    match self.buf_mut().undo() {
                        Some(p) => {
                            self.move_to(p);
                            done += 1;
                        }
                        None => break,
                    }
                }
                if done == 0 {
                    self.set_msg("Already at oldest change");
                }
            }
            "redo" => {
                self.no_dot = true;
                let mut done = 0;
                for _ in 0..n {
                    match self.buf_mut().redo() {
                        Some(p) => {
                            self.move_to(p);
                            done += 1;
                        }
                        None => break,
                    }
                }
                if done == 0 {
                    self.set_msg("Already at newest change");
                }
            }
            "J" | "gJ" => {
                if self.mode != Mode::Normal {
                    let (s, e) = order(self.visual_anchor, self.cursor());
                    self.leave_visual();
                    self.join_lines(s.0, (e.0 - s.0 + 1).max(2), name == "J");
                } else {
                    self.join_lines(self.cursor().0, n.max(2), name == "J");
                }
            }
            "~" => {
                let (r, c) = self.cursor();
                let len = self.line_len(r);
                if len == 0 {
                    return;
                }
                let e = (c + n).min(len);
                let nt: Vec<char> = self.lines()[r][c..e].iter().flat_map(|ch| map_case(Op::Toggle, *ch).chars().collect::<Vec<_>>()).collect();
                self.buf_mut().delete_range((r, c), (r, e));
                self.buf_mut().insert_text((r, c), &nt);
                self.move_to((r, e));
            }
            "." => {
                self.no_dot = true;
                if self.last_change.is_empty() || self.in_dot {
                    return;
                }
                let mut keys = self.last_change.clone();
                if let Some(cn) = count {
                    // a count given to `.` replaces the original one
                    let mut i = 0;
                    let mut prefix: Vec<Key> = Vec::new();
                    if keys.first() == Some(&Key::Char('"')) && keys.len() > 1 {
                        prefix.extend(keys[..2].iter().cloned());
                        i = 2;
                    }
                    while matches!(keys.get(i), Some(Key::Char(c)) if c.is_ascii_digit() && (*c != '0' || i > 0 && false)) {
                        i += 1;
                    }
                    let mut rest: Vec<Key> = keys[i..].to_vec();
                    if matches!(rest.first(), Some(Key::Char('d' | 'c' | 'y' | '<' | '>'))) {
                        let mut j = 1;
                        while matches!(rest.get(j), Some(Key::Char(c)) if c.is_ascii_digit() && (*c != '0' || j > 1)) {
                            j += 1;
                        }
                        rest.drain(1..j);
                    }
                    keys = prefix;
                    keys.extend(cn.to_string().chars().map(Key::Char));
                    keys.extend(rest);
                    self.last_change = keys.clone();
                }
                self.replay_keys(keys, true);
                // the inner commands reset the flag; `.` itself must never become the last change
                self.no_dot = true;
            }
            "i" => self.enter_insert(n, None),
            "a" => {
                let (r, c) = self.cursor();
                if self.line_len(r) > 0 {
                    self.docs[self.cur].cursor = (r, c + 1);
                }
                self.enter_insert(n, None);
            }
            "I" => {
                let r = self.cursor().0;
                let c = self.first_nonblank_idx(r);
                self.docs[self.cur].cursor = (r, c);
                self.enter_insert(n, None);
            }
            "gI" => {
                let r = self.cursor().0;
                self.docs[self.cur].cursor = (r, 0);
                self.enter_insert(n, None);
            }
            "gi" => {
                if let Some(p) = self.doc().marks.get(&'^').copied() {
                    let p = self.buf().clamp(p);
                    self.docs[self.cur].cursor = p;
                }
                self.enter_insert(n, None);
            }
            "A" => {
                let r = self.cursor().0;
                self.docs[self.cur].cursor = (r, self.line_len(r));
                self.enter_insert(n, None);
            }
            "o" | "O" => {
                self.open_line(name == "o");
                self.enter_insert(n, Some(name == "o"));
            }
            "v" => match self.mode {
                Mode::Visual => self.leave_visual(),
                Mode::VisualLine => self.mode = Mode::Visual,
                _ => self.enter_visual(Mode::Visual),
            },
            "V" => match self.mode {
                Mode::VisualLine => self.leave_visual(),
                Mode::Visual => self.mode = Mode::VisualLine,
                _ => self.enter_visual(Mode::VisualLine),
            },
            "Ctrl-v" => self.set_err("Block-wise visual mode is not supported"),
            "gv" => {
                if let Some((m, a, b)) = self.last_visual {
                    self.visual_anchor = self.buf().clamp(a);
                    self.mode = m;
                    let b = self.buf().clamp(b);
                    self.docs[self.cur].cursor = b;
                    self.clamp_cursor();
                }
            }
            "v_o" => {
                let a = self.visual_anchor;
                self.visual_anchor = self.cursor();
                self.docs[self.cur].cursor = a;
                self.update_want();
            }
            "v_r" => {
                let ch = arg.unwrap_or(' ');
                let region = self.visual_region(false);
                self.leave_visual();
                self.replace_region(region, ch);
            }
            "colon" => {
                if self.mode != Mode::Normal {
                    self.leave_visual();
                    self.start_cmdline(':', "'<,'>");
                } else {
                    let pre = if let Some(k) = count { if k == 1 { ".".to_string() } else { format!(".,.+{}", k - 1) } } else { String::new() };
                    self.start_cmdline(':', &pre);
                }
            }
            "slash" => self.start_cmdline('/', ""),
            "question" => self.start_cmdline('?', ""),
            "r" => {
                let ch = arg.unwrap_or(' ');
                let (r, c) = self.cursor();
                if c + n > self.line_len(r) {
                    return;
                }
                if ch == '\n' {
                    self.buf_mut().delete_range((r, c), (r, c + n));
                    let end = self.buf_mut().insert_text((r, c), &['\n']);
                    self.move_to(end);
                } else {
                    self.replace_region(Region::Chars { start: (r, c), end: (r, c + n) }, ch);
                    self.move_to((r, c + n - 1));
                }
            }
            "q" => {
                if let Some(c) = arg {
                    if c.is_ascii_alphanumeric() {
                        self.recording = Some(c);
                        self.macro_buf.clear();
                    }
                }
            }
            "@" => {
                self.no_dot = true;
                let c = arg.unwrap_or('@');
                let key = if c == '@' { self.last_macro_name() } else { Some(c) };
                if let Some(k) = key {
                    if let Some(keys) = self.macros.get(&k).cloned() {
                        self.set_last_macro(k);
                        if self.replay_depth > 20 {
                            self.set_err("macro recursion too deep");
                            return;
                        }
                        for _ in 0..n {
                            if self.timed_out() {
                                break;
                            }
                            self.replay_keys(keys.clone(), false);
                        }
                        self.no_dot = true;
                    } else {
                        self.set_err(format!("Register @{} is empty", k));
                    }
                }
            }
            "m" => {
                if let Some(c) = arg {
                    let p = self.cursor();
                    self.doc_mut().marks.insert(c, p);
                }
            }
            "Ctrl-d" | "Ctrl-u" => {
                let h = (self.view_h / 2).max(1);
                let last = self.buf().line_count() - 1;
                let (r, _) = self.cursor();
                let nr = if name == "Ctrl-d" { (r + h).min(last) } else { r.saturating_sub(h) };
                let top = self.doc().top;
                self.doc_mut().top = if name == "Ctrl-d" { (top + h).min(last) } else { top.saturating_sub(h) };
                let c = self.first_nonblank(nr);
                self.move_to((nr, c));
            }
            "Ctrl-f" | "Ctrl-b" => {
                let h = self.view_h.saturating_sub(2).max(1) * n;
                let last = self.buf().line_count() - 1;
                let (r, _) = self.cursor();
                let nr = if name == "Ctrl-f" { (r + h).min(last) } else { r.saturating_sub(h) };
                let top = self.doc().top;
                self.doc_mut().top = if name == "Ctrl-f" { (top + h).min(last) } else { top.saturating_sub(h) };
                let c = self.first_nonblank(nr);
                self.move_to((nr, c));
            }
            "Ctrl-e" | "Ctrl-y" => {
                let last = self.buf().line_count() - 1;
                let top = self.doc().top;
                let nt = if name == "Ctrl-e" { (top + n).min(last) } else { top.saturating_sub(n) };
                self.doc_mut().top = nt;
                let so = self.opts.scrolloff.min(self.view_h.saturating_sub(1) / 2);
                let (r, c) = self.cursor();
                let lo = nt + so;
                let hi = (nt + self.view_h).saturating_sub(1 + so);
                let nr = r.clamp(lo.min(last), hi.max(lo).min(last));
                if nr != r {
                    self.move_to((nr, c));
                }
            }
            "zz" | "zt" | "zb" => {
                let r = self.cursor().0;
                let h = self.view_h;
                self.doc_mut().top = match name {
                    "zt" => r,
                    "zb" => r.saturating_sub(h.saturating_sub(1)),
                    _ => r.saturating_sub(h / 2),
                };
            }
            "Ctrl-o" => self.jump_older(),
            "Tab" => self.jump_newer(),
            "Ctrl-a" | "Ctrl-x" => {
                let delta = if name == "Ctrl-a" { n as i64 } else { -(n as i64) };
                self.increment(delta);
            }
            "ZZ" => self.execute_ex("x"),
            "ZQ" => self.execute_ex("q!"),
            "files" => self.open_file_picker(),
            "ctrl-w" => match arg {
                Some('c') | Some('q') | Some('o') => self.close_terminal(),
                // the project panel is to the left of the text
                Some('h') => self.focus_sidebar(),
                Some('l') => self.unfocus_sidebar(),
                _ => {}
            },
            "gd" | "K" | "gr" | "]d" | "[d" => self.lsp_command(name),
            "paste" => {
                self.no_dot = true;
            }
            "&" => self.execute_ex("s"),
            _ => {}
        }
    }

    fn last_macro_name(&self) -> Option<char> {
        self.regs.get(&'\u{1}').and_then(|r| r.text.first().copied())
    }

    fn set_last_macro(&mut self, c: char) {
        self.regs.insert('\u{1}', crate::editor::Reg { text: vec![c], linewise: false });
    }

    fn replace_region(&mut self, region: Region, ch: char) {
        match region {
            Region::Lines { r0, r1 } => {
                for r in r0..=r1 {
                    let nl = vec![ch; self.line_len(r)];
                    self.buf_mut().set_line(r, nl);
                }
                self.move_to((r0, 0));
            }
            Region::Chars { start, end } => {
                let end = self.buf().clamp(end);
                let text = self.buf().get_range(start, end);
                let nt: Vec<char> = text.iter().map(|c| if *c == '\n' { '\n' } else { ch }).collect();
                self.buf_mut().delete_range(start, end);
                self.buf_mut().insert_text(start, &nt);
                self.move_to(start);
            }
        }
    }

    fn paste(&mut self, reg: Option<char>, n: usize, after: bool) {
        let Some(r) = self.get_reg(reg) else {
            self.set_err("Nothing in register");
            return;
        };
        let size = r.text.len().checked_add(1).and_then(|l| l.checked_mul(n));
        if !self.has_room(size) {
            return;
        }
        if r.linewise {
            let one: Vec<Vec<char>> = r.text.split(|c| *c == '\n').map(|s| s.to_vec()).collect();
            let mut all = Vec::new();
            for _ in 0..n {
                all.extend(one.iter().cloned());
            }
            let row = self.cursor().0;
            let at = if after { row + 1 } else { row };
            self.buf_mut().insert_lines(at, all);
            let c = self.first_nonblank(at);
            self.move_to((at, c));
        } else {
            let mut text = Vec::new();
            for _ in 0..n {
                text.extend(r.text.iter().copied());
            }
            if text.is_empty() {
                return;
            }
            let (row, col) = self.cursor();
            let len = self.line_len(row);
            let at = if after && len > 0 { (row, col + 1) } else { (row, col) };
            let end = self.buf_mut().insert_text(at, &text);
            if text.contains(&'\n') {
                self.move_to(at);
            } else {
                self.move_to((end.0, end.1.saturating_sub(1)));
            }
        }
    }

    fn visual_paste(&mut self, reg: Option<char>) {
        let Some(r) = self.get_reg(reg) else {
            self.set_err("Nothing in register");
            return;
        };
        let linewise_sel = self.mode == Mode::VisualLine;
        let region = self.visual_region(false);
        self.leave_visual();
        // delete the selection (this also fills the unnamed register, as in vim)
        match region {
            Region::Lines { r0, r1 } => {
                let t: Vec<char> = (r0..=r1)
                    .flat_map(|i| {
                        let mut l = self.lines()[i].clone();
                        if i < r1 {
                            l.push('\n');
                        }
                        l
                    })
                    .collect();
                self.set_reg(None, t, true, false);
                let lines: Vec<Vec<char>> = r.text.split(|c| *c == '\n').map(|s| s.to_vec()).collect();
                self.buf_mut().replace_line_range(r0, r1, lines);
                let c = self.first_nonblank(r0);
                self.move_to((r0, c));
            }
            Region::Chars { start, end } => {
                let end = self.buf().clamp(end);
                let t = self.buf().get_range(start, end);
                self.set_reg(None, t, false, false);
                self.buf_mut().delete_range(start, end);
                if r.linewise && !linewise_sel {
                    // linewise text into a charwise selection: split the line around it
                    let lines: Vec<char> = {
                        let mut v = vec!['\n'];
                        v.extend(r.text.iter().copied());
                        v.push('\n');
                        v
                    };
                    let e = self.buf_mut().insert_text(start, &lines);
                    self.move_to((e.0, 0));
                } else {
                    let e = self.buf_mut().insert_text(start, &r.text);
                    if r.text.contains(&'\n') {
                        self.move_to(start);
                    } else {
                        self.move_to((e.0, e.1.saturating_sub(1)));
                    }
                }
            }
        }
    }

    pub fn join_lines(&mut self, r: usize, count: usize, spaces: bool) {
        let last_row = self.buf().line_count() - 1;
        let end = (r + count.max(2) - 1).min(last_row);
        if end <= r {
            return;
        }
        let mut line = self.lines()[r].clone();
        let mut join_col = line.len();
        for next in r + 1..=end {
            let mut nl = self.lines()[next].clone();
            join_col = line.len();
            if spaces {
                let k = nl.iter().take_while(|c| c.is_whitespace()).count();
                nl.drain(..k);
                let need_space = !line.is_empty() && !line.last().is_some_and(|c| c.is_whitespace()) && !nl.is_empty() && nl[0] != ')';
                if need_space {
                    line.push(' ');
                }
            }
            line.extend(nl);
        }
        self.buf_mut().delete_lines(r + 1, end);
        self.buf_mut().set_line(r, line);
        self.move_to((r, join_col));
    }

    fn increment(&mut self, delta: i64) {
        let (r, c) = self.cursor();
        let line = self.lines()[r].clone();
        let mut i = c;
        if i >= line.len() {
            return;
        }
        // inside a number: back up to its start; otherwise find the next digit
        if line[i].is_ascii_digit() {
            while i > 0 && line[i - 1].is_ascii_digit() {
                i -= 1;
            }
        } else {
            while i < line.len() && !line[i].is_ascii_digit() {
                i += 1;
            }
            if i >= line.len() {
                return;
            }
        }
        let mut e = i;
        while e < line.len() && line[e].is_ascii_digit() {
            e += 1;
        }
        let neg = i > 0 && line[i - 1] == '-';
        let s: String = line[i..e].iter().collect();
        let Ok(v) = s.parse::<i64>() else { return };
        let v = if neg { -v } else { v };
        let nv = v.saturating_add(delta);
        let start = if neg { i - 1 } else { i };
        let ns = nv.to_string();
        let mut nl = line[..start].to_vec();
        nl.extend(ns.chars());
        nl.extend_from_slice(&line[e..]);
        let ncol = start + ns.chars().count() - 1;
        self.buf_mut().set_line(r, nl);
        self.move_to((r, ncol));
    }
}
