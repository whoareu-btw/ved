//! A small VT100/xterm-style terminal emulator: enough for shells, vim, less, htop, cargo.
//! It is pure data (no I/O): feed it bytes, read cells back. Responses the child expects
//! (cursor position reports, device attributes) are queued in `responses`.

use std::collections::VecDeque;

use crate::screen::{char_width, Cell, Color, Style, BOLD, DIM, ITALIC, REVERSE, STRIKE, UNDERLINE};

const MAX_SCROLLBACK: usize = 5000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Ground,
    Escape,
    EscInter, // ESC ( x  -- intermediate bytes then one final byte
    Csi,
    Osc,
    OscEsc,
    Ignore, // DCS / PM / APC until ST
    IgnoreEsc,
}

#[derive(Clone)]
struct Screen {
    cells: Vec<Cell>,
    cur: (usize, usize),
    saved: (usize, usize),
}

pub struct Vt {
    pub rows: usize,
    pub cols: usize,
    cells: Vec<Cell>,
    pub cur: (usize, usize),
    saved: (usize, usize),
    saved_style: Style,
    pending_wrap: bool,
    style: Style,
    top: usize,
    bottom: usize, // inclusive
    tabs: Vec<bool>,
    pub scrollback: VecDeque<Vec<Cell>>,
    alt: Option<Screen>,
    pub in_alt: bool,
    // modes
    pub autowrap: bool,
    pub origin: bool,
    pub app_cursor: bool,
    pub cursor_visible: bool,
    pub bracketed_paste: bool,
    pub mouse: bool,
    /// the program also wants reports of the mouse moving with a button held (mode 1002 or 1003)
    pub mouse_motion: bool,
    /// the program asked for SGR mouse reports (mode 1006), the only kind ved forwards
    pub mouse_sgr: bool,
    insert_mode: bool,
    // charsets
    g0_graphics: bool,
    g1_graphics: bool,
    shifted: bool,
    // parser
    state: State,
    params: Vec<u32>,
    cur_param: Option<u32>,
    private: bool,
    inter: Vec<u8>,
    osc: Vec<u8>,
    utf8: Vec<u8>,
    last_char: Option<char>,
    pub title: String,
    /// The default background colour a program asked for with OSC 11 (as it was written), until OSC 111.
    pub default_bg: Option<String>,
    pub responses: Vec<u8>,
    pub bell: bool,
}

fn blank(style: Style) -> Cell {
    Cell { ch: ' ', style: Style { fg: Color::Default, bg: style.bg, attrs: 0 } }
}

fn dec_graphics(c: char) -> char {
    match c {
        'q' => '\u{2500}',
        'x' => '\u{2502}',
        'l' => '\u{250c}',
        'k' => '\u{2510}',
        'm' => '\u{2514}',
        'j' => '\u{2518}',
        't' => '\u{251c}',
        'u' => '\u{2524}',
        'v' => '\u{2534}',
        'w' => '\u{252c}',
        'n' => '\u{253c}',
        'a' => '\u{2592}',
        '`' => '\u{25c6}',
        'f' => '\u{b0}',
        'g' => '\u{b1}',
        'o' | 'p' => '\u{2500}',
        'r' | 's' => '\u{2500}',
        '~' => '\u{b7}',
        _ => c,
    }
}

impl Vt {
    pub fn new(rows: usize, cols: usize) -> Vt {
        let rows = rows.max(1);
        let cols = cols.max(1);
        let mut v = Vt {
            rows,
            cols,
            cells: vec![Cell::BLANK; rows * cols],
            cur: (0, 0),
            saved: (0, 0),
            saved_style: Style::DEFAULT,
            pending_wrap: false,
            style: Style::DEFAULT,
            top: 0,
            bottom: rows - 1,
            tabs: Vec::new(),
            scrollback: VecDeque::new(),
            alt: None,
            in_alt: false,
            autowrap: true,
            origin: false,
            app_cursor: false,
            cursor_visible: true,
            bracketed_paste: false,
            mouse: false,
            mouse_motion: false,
            mouse_sgr: false,
            insert_mode: false,
            g0_graphics: false,
            g1_graphics: false,
            shifted: false,
            state: State::Ground,
            params: Vec::new(),
            cur_param: None,
            private: false,
            inter: Vec::new(),
            osc: Vec::new(),
            utf8: Vec::new(),
            last_char: None,
            title: String::new(),
            default_bg: None,
            responses: Vec::new(),
            bell: false,
        };
        v.reset_tabs();
        v
    }

    fn reset_tabs(&mut self) {
        self.tabs = (0..self.cols).map(|c| c % 8 == 0).collect();
    }

    pub fn cell(&self, r: usize, c: usize) -> Cell {
        self.cells[r * self.cols + c]
    }

    /// Row `r` of the view that is `offset` lines scrolled back into history.
    pub fn view_row(&self, r: usize, offset: usize) -> Vec<Cell> {
        let sb = self.scrollback.len();
        let offset = offset.min(sb);
        // line index in the combined (scrollback ++ screen) buffer
        let idx = sb - offset + r;
        if idx < sb {
            let mut row = self.scrollback[idx].clone();
            row.resize(self.cols, Cell::BLANK);
            row
        } else {
            let sr = idx - sb;
            if sr < self.rows {
                self.cells[sr * self.cols..(sr + 1) * self.cols].to_vec()
            } else {
                vec![Cell::BLANK; self.cols]
            }
        }
    }

    pub fn row_text(&self, r: usize) -> String {
        let mut s = String::new();
        for c in 0..self.cols {
            let ch = self.cells[r * self.cols + c].ch;
            if ch != '\0' {
                s.push(ch);
            }
        }
        s.trim_end().to_string()
    }

    pub fn text(&self) -> String {
        (0..self.rows).map(|r| self.row_text(r)).collect::<Vec<_>>().join("\n")
    }

    // ---- resize ----------------------------------------------------------------

    pub fn resize(&mut self, rows: usize, cols: usize) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if rows == self.rows && cols == self.cols {
            return;
        }
        // rows: keep the line with the cursor visible; push displaced top lines to scrollback
        let mut grid: Vec<Vec<Cell>> = (0..self.rows).map(|r| self.cells[r * self.cols..(r + 1) * self.cols].to_vec()).collect();
        let mut cur = self.cur;
        if rows < self.rows {
            let mut drop_top = 0;
            while grid.len() > rows && cur.0 > 0 && drop_top < self.rows {
                let row = grid.remove(0);
                if !self.in_alt {
                    self.push_scrollback(row);
                }
                cur.0 -= 1;
                drop_top += 1;
            }
            grid.truncate(rows);
        } else {
            while grid.len() < rows {
                grid.push(vec![Cell::BLANK; self.cols]);
            }
        }
        for row in grid.iter_mut() {
            row.resize(cols, Cell::BLANK);
        }
        self.cells = grid.into_iter().flatten().collect();
        self.rows = rows;
        self.cols = cols;
        self.cur = (cur.0.min(rows - 1), cur.1.min(cols - 1));
        self.saved = (self.saved.0.min(rows - 1), self.saved.1.min(cols - 1));
        self.top = 0;
        self.bottom = rows - 1;
        self.pending_wrap = false;
        self.reset_tabs();
        if let Some(a) = self.alt.as_mut() {
            a.cells = vec![Cell::BLANK; rows * cols];
            a.cur = (0, 0);
        }
    }

    fn push_scrollback(&mut self, mut row: Vec<Cell>) {
        while row.last() == Some(&Cell::BLANK) {
            row.pop();
        }
        self.scrollback.push_back(row);
        if self.scrollback.len() > MAX_SCROLLBACK {
            self.scrollback.pop_front();
        }
    }

    // ---- primitive screen operations ---------------------------------------------

    fn clear_cells(&mut self, r: usize, c0: usize, c1: usize) {
        let b = blank(self.style);
        for c in c0..c1.min(self.cols) {
            self.cells[r * self.cols + c] = b;
        }
    }

    fn scroll_up(&mut self, n: usize) {
        let (top, bot) = (self.top, self.bottom);
        let n = n.min(bot + 1 - top);
        for _ in 0..n {
            if top == 0 && !self.in_alt {
                let row = self.cells[0..self.cols].to_vec();
                self.push_scrollback(row);
            }
            for r in top..bot {
                let (a, b) = ((r + 1) * self.cols, (r + 2) * self.cols);
                self.cells.copy_within(a..b, r * self.cols);
            }
            self.clear_cells(bot, 0, self.cols);
        }
    }

    fn scroll_down(&mut self, n: usize) {
        let (top, bot) = (self.top, self.bottom);
        let n = n.min(bot + 1 - top);
        for _ in 0..n {
            for r in (top + 1..=bot).rev() {
                let (a, b) = ((r - 1) * self.cols, r * self.cols);
                self.cells.copy_within(a..b, r * self.cols);
            }
            self.clear_cells(top, 0, self.cols);
        }
    }

    fn linefeed(&mut self) {
        if self.cur.0 == self.bottom {
            self.scroll_up(1);
        } else if self.cur.0 + 1 < self.rows {
            self.cur.0 += 1;
        }
    }

    fn reverse_index(&mut self) {
        if self.cur.0 == self.top {
            self.scroll_down(1);
        } else if self.cur.0 > 0 {
            self.cur.0 -= 1;
        }
    }

    fn put(&mut self, ch: char) {
        let ch = if (self.shifted && self.g1_graphics) || (!self.shifted && self.g0_graphics) { dec_graphics(ch) } else { ch };
        let mut w = char_width(ch);
        if w == 0 {
            return;
        }
        let mut ch = ch;
        if w == 2 && self.cols < 2 {
            // nothing wide fits in a one-column screen
            w = 1;
            ch = '?';
        }
        if self.pending_wrap {
            if self.autowrap {
                self.cur.1 = 0;
                self.linefeed();
            }
            self.pending_wrap = false;
        }
        if w == 2 && self.cur.1 + 1 >= self.cols {
            // wide char does not fit on this line
            if self.autowrap {
                self.cells[self.cur.0 * self.cols + self.cur.1] = blank(self.style);
                self.cur.1 = 0;
                self.linefeed();
            } else {
                return;
            }
        }
        let (r, c) = self.cur;
        if self.insert_mode {
            let base = r * self.cols;
            for k in (c + w..self.cols).rev() {
                self.cells[base + k] = self.cells[base + k - w];
            }
        }
        let st = self.style;
        self.cells[r * self.cols + c] = Cell { ch, style: st };
        if w == 2 {
            self.cells[r * self.cols + c + 1] = Cell { ch: '\0', style: st };
        }
        self.last_char = Some(ch);
        if c + w >= self.cols {
            self.pending_wrap = true;
            self.cur.1 = self.cols - 1;
        } else {
            self.cur.1 = c + w;
        }
    }

    fn move_to(&mut self, r: usize, c: usize) {
        self.pending_wrap = false;
        let (rmin, rmax) = if self.origin { (self.top, self.bottom) } else { (0, self.rows - 1) };
        let r = if self.origin { self.top + r } else { r };
        self.cur = (r.clamp(rmin, rmax), c.min(self.cols - 1));
    }

    fn enter_alt(&mut self, save_cursor: bool) {
        if self.in_alt {
            return;
        }
        let saved = Screen { cells: std::mem::replace(&mut self.cells, vec![Cell::BLANK; self.rows * self.cols]), cur: self.cur, saved: self.saved };
        self.alt = Some(saved);
        self.in_alt = true;
        if save_cursor {
            self.cur = (0, 0);
        }
        self.top = 0;
        self.bottom = self.rows - 1;
    }

    fn leave_alt(&mut self) {
        if let Some(s) = self.alt.take() {
            // the main screen may have been resized while the alt screen was up
            if s.cells.len() == self.rows * self.cols {
                self.cells = s.cells;
                self.cur = (s.cur.0.min(self.rows - 1), s.cur.1.min(self.cols - 1));
            } else {
                self.cells = vec![Cell::BLANK; self.rows * self.cols];
            }
            self.saved = s.saved;
            self.pending_wrap = false;
        }
        self.in_alt = false;
        self.top = 0;
        self.bottom = self.rows - 1;
    }

    fn full_reset(&mut self) {
        let (rows, cols) = (self.rows, self.cols);
        let sb = std::mem::take(&mut self.scrollback);
        *self = Vt::new(rows, cols);
        self.scrollback = sb;
    }

    // ---- input -----------------------------------------------------------------------

    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.byte(b);
        }
    }

    fn byte(&mut self, b: u8) {
        match self.state {
            State::Ground => self.ground(b),
            State::Escape => self.escape(b),
            State::EscInter => {
                // ESC <inter> <final>: only charset designation matters
                if (0x20..=0x2f).contains(&b) {
                    self.inter.push(b);
                } else {
                    let g = self.inter.first().copied();
                    let graphics = b == b'0';
                    match g {
                        Some(b'(') => self.g0_graphics = graphics,
                        Some(b')') => self.g1_graphics = graphics,
                        _ => {}
                    }
                    self.inter.clear();
                    self.state = State::Ground;
                }
            }
            State::Csi => self.csi_byte(b),
            State::Osc => match b {
                0x07 => self.osc_end(),
                0x1b => self.state = State::OscEsc,
                _ => {
                    if self.osc.len() < 4096 {
                        self.osc.push(b);
                    }
                }
            },
            State::OscEsc => {
                if b == b'\\' {
                    self.osc_end();
                } else {
                    self.osc_end();
                    self.escape(b);
                }
            }
            State::Ignore => match b {
                0x07 => self.state = State::Ground,
                0x1b => self.state = State::IgnoreEsc,
                _ => {}
            },
            State::IgnoreEsc => {
                self.state = if b == b'\\' { State::Ground } else { State::Ignore };
            }
        }
    }

    fn osc_end(&mut self) {
        let s = String::from_utf8_lossy(&self.osc).to_string();
        self.osc.clear();
        if let Some((n, rest)) = s.split_once(';') {
            if n == "0" || n == "2" {
                self.title = rest.to_string();
            } else if n == "11" && rest != "?" {
                self.default_bg = Some(rest.to_string());
            }
        } else if s == "111" {
            self.default_bg = None;
        }
        self.state = State::Ground;
    }

    fn ground(&mut self, b: u8) {
        if !self.utf8.is_empty() {
            self.utf8.push(b);
            let need = match self.utf8[0] {
                0xC0..=0xDF => 2,
                0xE0..=0xEF => 3,
                _ => 4,
            };
            if b & 0xC0 != 0x80 {
                // broken sequence: emit a replacement and reprocess this byte
                self.utf8.clear();
                self.put('\u{FFFD}');
                self.ground(b);
                return;
            }
            if self.utf8.len() == need {
                let s = String::from_utf8_lossy(&self.utf8).to_string();
                self.utf8.clear();
                if let Some(c) = s.chars().next() {
                    self.put(c);
                }
            }
            return;
        }
        match b {
            0x00 | 0x7f => {}
            0x07 => self.bell = true,
            0x08 => {
                self.pending_wrap = false;
                if self.cur.1 > 0 {
                    self.cur.1 -= 1;
                }
            }
            0x09 => {
                self.pending_wrap = false;
                let mut c = self.cur.1 + 1;
                while c < self.cols - 1 && !self.tabs[c] {
                    c += 1;
                }
                self.cur.1 = c.min(self.cols - 1);
            }
            0x0a..=0x0c => {
                self.pending_wrap = false;
                self.linefeed();
            }
            0x0d => {
                self.pending_wrap = false;
                self.cur.1 = 0;
            }
            0x0e => self.shifted = true,
            0x0f => self.shifted = false,
            0x1b => self.state = State::Escape,
            0x20..=0x7e => self.put(b as char),
            0xC0..=0xF7 => self.utf8.push(b),
            _ => {}
        }
    }

    fn escape(&mut self, b: u8) {
        self.state = State::Ground;
        match b {
            b'[' => {
                self.state = State::Csi;
                self.params.clear();
                self.cur_param = None;
                self.private = false;
                self.inter.clear();
            }
            b']' => {
                self.state = State::Osc;
                self.osc.clear();
            }
            b'P' | b'X' | b'^' | b'_' => self.state = State::Ignore,
            b'(' | b')' | b'*' | b'+' | b'#' | b'%' | b' ' => {
                self.state = State::EscInter;
                self.inter.clear();
                self.inter.push(b);
            }
            b'7' => {
                self.saved = self.cur;
                self.saved_style = self.style;
            }
            b'8' => {
                self.cur = (self.saved.0.min(self.rows - 1), self.saved.1.min(self.cols - 1));
                self.style = self.saved_style;
                self.pending_wrap = false;
            }
            b'D' => self.linefeed(),
            b'E' => {
                self.cur.1 = 0;
                self.linefeed();
            }
            b'M' => self.reverse_index(),
            b'H' => {
                if self.cur.1 < self.tabs.len() {
                    self.tabs[self.cur.1] = true;
                }
            }
            b'c' => self.full_reset(),
            _ => {}
        }
    }

    fn csi_byte(&mut self, b: u8) {
        match b {
            b'0'..=b'9' => {
                let d = (b - b'0') as u32;
                self.cur_param = Some(self.cur_param.unwrap_or(0).saturating_mul(10).saturating_add(d).min(1_000_000));
            }
            b';' | b':' => {
                self.params.push(self.cur_param.take().unwrap_or(0));
            }
            b'?' | b'>' | b'<' | b'=' => self.private = b == b'?',
            0x20..=0x2f => self.inter.push(b),
            0x40..=0x7e => {
                if let Some(p) = self.cur_param.take() {
                    self.params.push(p);
                } else if !self.params.is_empty() {
                    self.params.push(0);
                }
                self.state = State::Ground;
                self.csi_dispatch(b);
            }
            0x1b => self.state = State::Escape,
            0x00..=0x1f => self.ground(b), // C0 controls inside a CSI still act
            _ => self.state = State::Ground,
        }
    }

    fn p(&self, i: usize, default: usize) -> usize {
        match self.params.get(i) {
            Some(&0) | None => default,
            Some(&v) => v as usize,
        }
    }

    fn csi_dispatch(&mut self, fin: u8) {
        if !self.inter.is_empty() {
            return; // e.g. cursor style (CSI Ps SP q): ignored
        }
        let rows = self.rows;
        let cols = self.cols;
        match fin {
            b'A' => {
                self.pending_wrap = false;
                let n = self.p(0, 1);
                let min = if self.cur.0 >= self.top { self.top } else { 0 };
                self.cur.0 = self.cur.0.saturating_sub(n).max(min);
            }
            b'B' | b'e' => {
                self.pending_wrap = false;
                let n = self.p(0, 1);
                let max = if self.cur.0 <= self.bottom { self.bottom } else { rows - 1 };
                self.cur.0 = (self.cur.0 + n).min(max);
            }
            b'C' | b'a' => {
                self.pending_wrap = false;
                self.cur.1 = (self.cur.1 + self.p(0, 1)).min(cols - 1);
            }
            b'D' => {
                self.pending_wrap = false;
                self.cur.1 = self.cur.1.saturating_sub(self.p(0, 1));
            }
            b'E' => {
                self.pending_wrap = false;
                self.cur = ((self.cur.0 + self.p(0, 1)).min(rows - 1), 0);
            }
            b'F' => {
                self.pending_wrap = false;
                self.cur = (self.cur.0.saturating_sub(self.p(0, 1)), 0);
            }
            b'G' | b'`' => {
                self.pending_wrap = false;
                self.cur.1 = (self.p(0, 1) - 1).min(cols - 1);
            }
            b'H' | b'f' => {
                let (r, c) = (self.p(0, 1) - 1, self.p(1, 1) - 1);
                self.move_to(r, c);
            }
            b'd' => {
                let r = self.p(0, 1) - 1;
                let c = self.cur.1;
                self.move_to(r, c);
            }
            b'J' => {
                let mode = self.params.first().copied().unwrap_or(0);
                let (r, c) = self.cur;
                match mode {
                    0 => {
                        self.clear_cells(r, c, cols);
                        for rr in r + 1..rows {
                            self.clear_cells(rr, 0, cols);
                        }
                    }
                    1 => {
                        for rr in 0..r {
                            self.clear_cells(rr, 0, cols);
                        }
                        self.clear_cells(r, 0, c + 1);
                    }
                    2 => {
                        for rr in 0..rows {
                            self.clear_cells(rr, 0, cols);
                        }
                    }
                    3 => self.scrollback.clear(),
                    _ => {}
                }
            }
            b'K' => {
                let mode = self.params.first().copied().unwrap_or(0);
                let (r, c) = self.cur;
                match mode {
                    0 => self.clear_cells(r, c, cols),
                    1 => self.clear_cells(r, 0, c + 1),
                    2 => self.clear_cells(r, 0, cols),
                    _ => {}
                }
            }
            b'L' => {
                if self.cur.0 >= self.top && self.cur.0 <= self.bottom {
                    let saved_top = self.top;
                    self.top = self.cur.0;
                    self.scroll_down(self.p(0, 1));
                    self.top = saved_top;
                }
            }
            b'M' => {
                if self.cur.0 >= self.top && self.cur.0 <= self.bottom {
                    let saved_top = self.top;
                    self.top = self.cur.0;
                    // lines deleted inside the region never go to scrollback
                    let was_alt = self.in_alt;
                    self.in_alt = true;
                    self.scroll_up(self.p(0, 1));
                    self.in_alt = was_alt;
                    self.top = saved_top;
                }
            }
            b'@' => {
                let (r, c) = self.cur;
                let n = self.p(0, 1).min(cols - c);
                let base = r * cols;
                for k in (c + n..cols).rev() {
                    self.cells[base + k] = self.cells[base + k - n];
                }
                self.clear_cells(r, c, c + n);
            }
            b'P' => {
                let (r, c) = self.cur;
                let n = self.p(0, 1).min(cols - c);
                let base = r * cols;
                for k in c..cols - n {
                    self.cells[base + k] = self.cells[base + k + n];
                }
                self.clear_cells(r, cols - n, cols);
            }
            b'X' => {
                let (r, c) = self.cur;
                self.clear_cells(r, c, c + self.p(0, 1));
            }
            b'S' => self.scroll_up(self.p(0, 1)),
            b'T' => self.scroll_down(self.p(0, 1)),
            b'b' => {
                if let Some(ch) = self.last_char {
                    for _ in 0..self.p(0, 1).min(10_000) {
                        self.put(ch);
                    }
                }
            }
            b'r' => {
                let t = self.p(0, 1) - 1;
                let b = self.p(1, rows) - 1;
                if t < b && b < rows {
                    self.top = t;
                    self.bottom = b;
                    self.move_to(0, 0);
                }
            }
            b's' if !self.private => {
                self.saved = self.cur;
            }
            b'u' => {
                self.cur = (self.saved.0.min(rows - 1), self.saved.1.min(cols - 1));
                self.pending_wrap = false;
            }
            b'm' => self.sgr(),
            b'h' | b'l' => {
                let on = fin == b'h';
                let params = if self.params.is_empty() { vec![0] } else { self.params.clone() };
                for p in params {
                    if self.private {
                        match p {
                            1 => self.app_cursor = on,
                            6 => {
                                self.origin = on;
                                self.move_to(0, 0);
                            }
                            7 => self.autowrap = on,
                            25 => self.cursor_visible = on,
                            47 | 1047 => {
                                if on {
                                    self.enter_alt(false)
                                } else {
                                    self.leave_alt()
                                }
                            }
                            1049 => {
                                if on {
                                    self.saved = self.cur;
                                    self.enter_alt(true)
                                } else {
                                    self.leave_alt();
                                    self.cur = (self.saved.0.min(rows - 1), self.saved.1.min(cols - 1));
                                }
                            }
                            2004 => self.bracketed_paste = on,
                            1000 => self.mouse = on,
                            1002 | 1003 => {
                                self.mouse = on;
                                self.mouse_motion = on;
                            }
                            1006 => self.mouse_sgr = on,
                            _ => {}
                        }
                    } else if p == 4 {
                        self.insert_mode = on;
                    }
                }
            }
            b'n' => match self.p(0, 0) {
                5 => self.responses.extend_from_slice(b"\x1b[0n"),
                6 => {
                    let r = self.cur.0 + 1 - if self.origin { self.top } else { 0 };
                    let s = format!("\x1b[{};{}R", r, self.cur.1 + 1);
                    self.responses.extend_from_slice(s.as_bytes());
                }
                _ => {}
            },
            b'c' => {
                if self.private {
                    return;
                }
                self.responses.extend_from_slice(b"\x1b[?62;22c");
            }
            _ => {}
        }
    }

    fn sgr(&mut self) {
        if self.params.is_empty() {
            self.style = Style::DEFAULT;
            return;
        }
        let ps = self.params.clone();
        let mut i = 0;
        while i < ps.len() {
            let n = ps[i];
            match n {
                0 => self.style = Style::DEFAULT,
                1 => self.style.attrs |= BOLD,
                2 => self.style.attrs |= DIM,
                3 => self.style.attrs |= ITALIC,
                4 => self.style.attrs |= UNDERLINE,
                7 => self.style.attrs |= REVERSE,
                9 => self.style.attrs |= STRIKE,
                22 => self.style.attrs &= !(BOLD | DIM),
                23 => self.style.attrs &= !ITALIC,
                24 => self.style.attrs &= !UNDERLINE,
                27 => self.style.attrs &= !REVERSE,
                29 => self.style.attrs &= !STRIKE,
                30..=37 => self.style.fg = Color::Idx((n - 30) as u8),
                39 => self.style.fg = Color::Default,
                40..=47 => self.style.bg = Color::Idx((n - 40) as u8),
                49 => self.style.bg = Color::Default,
                90..=97 => self.style.fg = Color::Idx((n - 90 + 8) as u8),
                100..=107 => self.style.bg = Color::Idx((n - 100 + 8) as u8),
                38 | 48 => {
                    let color = match ps.get(i + 1) {
                        Some(5) => {
                            let c = ps.get(i + 2).copied().unwrap_or(0).min(255) as u8;
                            i += 2;
                            Some(Color::Idx(c))
                        }
                        Some(2) => {
                            let g = |k: usize| ps.get(i + k).copied().unwrap_or(0).min(255) as u8;
                            let c = Color::Rgb(g(2), g(3), g(4));
                            i += 4;
                            Some(c)
                        }
                        _ => None,
                    };
                    if let Some(c) = color {
                        if n == 38 {
                            self.style.fg = c;
                        } else {
                            self.style.bg = c;
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vt(rows: usize, cols: usize, input: &str) -> Vt {
        let mut v = Vt::new(rows, cols);
        v.feed(input.as_bytes());
        v
    }

    #[test]
    fn plain_text_and_newlines() {
        let v = vt(3, 10, "hello\r\nworld");
        assert_eq!(v.row_text(0), "hello");
        assert_eq!(v.row_text(1), "world");
        assert_eq!(v.cur, (1, 5));
    }

    #[test]
    fn wraps_and_scrolls_into_history() {
        let v = vt(2, 4, "abcdefgh\r\nxy");
        assert_eq!(v.row_text(0), "efgh");
        assert_eq!(v.row_text(1), "xy");
        assert_eq!(v.scrollback.len(), 1);
        assert_eq!(v.view_row(0, 1).iter().map(|c| c.ch).collect::<String>().trim_end(), "abcd");
    }

    #[test]
    fn deferred_wrap_at_last_column() {
        let v = vt(2, 3, "abc");
        assert_eq!(v.cur, (0, 2)); // still on the first line until the next char
        let v = vt(2, 3, "abcd");
        assert_eq!(v.row_text(1), "d");
    }

    #[test]
    fn cursor_movement_and_erase() {
        let v = vt(3, 10, "abcdef\x1b[3D\x1b[K");
        assert_eq!(v.row_text(0), "abc");
        let v = vt(3, 10, "\x1b[2;4Hx");
        assert_eq!(v.cell(1, 3).ch, 'x');
        let v = vt(3, 5, "aaaaa\r\nbbbbb\r\nccccc\x1b[2J");
        assert_eq!(v.text(), "\n\n");
        let v = vt(3, 5, "abcde\x1b[1;3H\x1b[P");
        assert_eq!(v.row_text(0), "abde");
        let v = vt(3, 5, "abcde\x1b[1;3H\x1b[2@");
        assert_eq!(v.row_text(0), "ab  c");
    }

    #[test]
    fn colours_and_attributes() {
        let v = vt(1, 10, "\x1b[1;31mA\x1b[0mB\x1b[38;5;200mC\x1b[48;2;1;2;3mD");
        assert_eq!(v.cell(0, 0).style.fg, Color::Idx(1));
        assert!(v.cell(0, 0).style.attrs & BOLD != 0);
        assert_eq!(v.cell(0, 1).style, Style::DEFAULT);
        assert_eq!(v.cell(0, 2).style.fg, Color::Idx(200));
        assert_eq!(v.cell(0, 3).style.bg, Color::Rgb(1, 2, 3));
    }

    #[test]
    fn scroll_region_and_insert_delete_lines() {
        let v = vt(4, 3, "a\r\nb\r\nc\r\nd\x1b[2;3r\x1b[2;1H\x1b[M");
        assert_eq!(v.row_text(0), "a");
        assert_eq!(v.row_text(1), "c");
        assert_eq!(v.row_text(2), "");
        assert_eq!(v.row_text(3), "d");
        let v = vt(3, 3, "a\r\nb\r\nc\x1b[2;1H\x1b[L");
        assert_eq!(v.row_text(1), "");
        assert_eq!(v.row_text(2), "b");
    }

    #[test]
    fn alt_screen_restores_main() {
        let v = vt(2, 6, "main\x1b[?1049hALT\x1b[?1049l");
        assert_eq!(v.row_text(0), "main");
        let v = vt(2, 6, "main\x1b[?1049hALT");
        assert_eq!(v.row_text(0), "ALT");
        assert!(v.in_alt);
    }

    #[test]
    fn utf8_and_wide_chars() {
        let v = vt(1, 10, "h\u{e9}\u{4e2d}x");
        assert_eq!(v.cell(0, 1).ch, '\u{e9}');
        assert_eq!(v.cell(0, 2).ch, '\u{4e2d}');
        assert_eq!(v.cell(0, 3).ch, '\0');
        assert_eq!(v.cell(0, 4).ch, 'x');
        // split across feeds
        let mut v = Vt::new(1, 5);
        v.feed(&[0xe4, 0xb8]);
        v.feed(&[0xad]);
        assert_eq!(v.cell(0, 0).ch, '\u{4e2d}');
    }

    #[test]
    fn device_reports() {
        let mut v = vt(5, 10, "\x1b[3;4H");
        v.feed(b"\x1b[6n");
        assert_eq!(v.responses, b"\x1b[3;4R");
        let mut v = Vt::new(2, 2);
        v.feed(b"\x1b[c");
        assert!(v.responses.starts_with(b"\x1b[?"));
    }

    #[test]
    fn osc_default_background_is_remembered_and_reset() {
        let mut v = vt(1, 10, "\x1b]11;rgb:1c/1c/1c\x1b\\x");
        assert_eq!(v.default_bg.as_deref(), Some("rgb:1c/1c/1c"));
        v.feed(b"\x1b]11;?\x07"); // a query is not a change
        assert_eq!(v.default_bg.as_deref(), Some("rgb:1c/1c/1c"));
        v.feed(b"\x1b]111\x1b\\");
        assert_eq!(v.default_bg, None);
        assert_eq!(v.cell(0, 0).ch, 'x'); // and none of it ended up on the screen
    }

    #[test]
    fn osc_title_and_line_drawing() {
        let v = vt(1, 10, "\x1b]0;my title\x07x");
        assert_eq!(v.title, "my title");
        let v = vt(1, 10, "\x1b(0lqk\x1b(B");
        assert_eq!(v.row_text(0), "\u{250c}\u{2500}\u{2510}");
    }

    #[test]
    fn resize_keeps_cursor_line() {
        let mut v = vt(4, 5, "1\r\n2\r\n3\r\n4");
        v.resize(2, 5);
        assert_eq!(v.row_text(1), "4");
        assert_eq!(v.scrollback.len(), 2);
        v.resize(6, 8);
        assert_eq!((v.rows, v.cols), (6, 8));
    }
}
