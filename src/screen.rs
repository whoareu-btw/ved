//! Cell grid + a row-diffing renderer that emits minimal escape sequences.

use std::fmt::Write as _;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Color {
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

pub const BOLD: u8 = 1;
pub const DIM: u8 = 2;
pub const ITALIC: u8 = 4;
pub const UNDERLINE: u8 = 8;
pub const REVERSE: u8 = 16;
pub const STRIKE: u8 = 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub attrs: u8,
}

impl Style {
    pub const DEFAULT: Style = Style { fg: Color::Default, bg: Color::Default, attrs: 0 };
    pub fn fg(c: Color) -> Style {
        Style { fg: c, ..Style::DEFAULT }
    }
    pub fn with_fg(mut self, c: Color) -> Style {
        self.fg = c;
        self
    }
    pub fn with_bg(mut self, c: Color) -> Style {
        self.bg = c;
        self
    }
    pub fn with(mut self, attr: u8) -> Style {
        self.attrs |= attr;
        self
    }
}

impl Default for Style {
    fn default() -> Style {
        Style::DEFAULT
    }
}

/// A single screen cell. `ch == '\0'` marks the right half of a wide character.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub ch: char,
    pub style: Style,
}

impl Cell {
    pub const BLANK: Cell = Cell { ch: ' ', style: Style::DEFAULT };
}

/// What may actually be sent to the terminal for `c`. Control characters (C0, DEL and the C1 range
/// U+0080..=U+009F, which some terminals treat as 8-bit escape introducers) never reach the
/// terminal as themselves: text from files, language servers and child processes could otherwise
/// drive the user's terminal. `'\0'` stays: it marks the right half of a wide character.
pub fn printable(c: char) -> char {
    let u = c as u32;
    if u >= 0x20 && u != 0x7f && !(0x80..=0x9f).contains(&u) || c == '\0' {
        c
    } else {
        '\u{fffd}'
    }
}

/// Display width of a char: 0 (combining / zero width), 1 or 2.
pub fn char_width(c: char) -> usize {
    let u = c as u32;
    if u < 0x300 {
        return 1;
    }
    if (0x0300..=0x036F).contains(&u)
        || (0x1AB0..=0x1AFF).contains(&u)
        || (0x200B..=0x200F).contains(&u)
        || (0x20D0..=0x20FF).contains(&u)
        || (0xFE00..=0xFE0F).contains(&u)
        || u == 0xFEFF
    {
        return 0;
    }
    if (0x1100..=0x115F).contains(&u)
        || (0x2E80..=0x303E).contains(&u)
        || (0x3041..=0x33FF).contains(&u)
        || (0x3400..=0x4DBF).contains(&u)
        || (0x4E00..=0x9FFF).contains(&u)
        || (0xA000..=0xA4CF).contains(&u)
        || (0xAC00..=0xD7A3).contains(&u)
        || (0xF900..=0xFAFF).contains(&u)
        || (0xFE30..=0xFE6F).contains(&u)
        || (0xFF00..=0xFF60).contains(&u)
        || (0xFFE0..=0xFFE6).contains(&u)
        || (0x1F300..=0x1F64F).contains(&u)
        || (0x1F900..=0x1F9FF).contains(&u)
        || (0x20000..=0x3FFFD).contains(&u)
    {
        return 2;
    }
    1
}

#[derive(Clone)]
pub struct Frame {
    pub rows: usize,
    pub cols: usize,
    pub cells: Vec<Cell>,
    /// Screen position of the hardware cursor, if it should be shown.
    pub cursor: Option<(usize, usize)>,
    /// 0 = block, 1 = bar, 2 = underline
    pub cursor_shape: u8,
}

impl Frame {
    pub fn new(rows: usize, cols: usize) -> Frame {
        Frame { rows, cols, cells: vec![Cell::BLANK; rows * cols], cursor: None, cursor_shape: 0 }
    }

    pub fn clear(&mut self) {
        for c in self.cells.iter_mut() {
            *c = Cell::BLANK;
        }
        self.cursor = None;
    }

    pub fn get(&self, r: usize, c: usize) -> Cell {
        self.cells[r * self.cols + c]
    }

    pub fn set(&mut self, r: usize, c: usize, ch: char, style: Style) {
        if r < self.rows && c < self.cols {
            self.cells[r * self.cols + c] = Cell { ch: printable(ch), style };
        }
    }

    pub fn set_style(&mut self, r: usize, c: usize, style: Style) {
        if r < self.rows && c < self.cols {
            self.cells[r * self.cols + c].style = style;
        }
    }

    /// Draw text starting at (r, c), clipped at `max_col` (exclusive). Returns the next column.
    pub fn put_str(&mut self, r: usize, mut c: usize, s: &str, style: Style, max_col: usize) -> usize {
        let max_col = max_col.min(self.cols);
        for ch in s.chars() {
            let w = char_width(ch);
            if w == 0 {
                continue;
            }
            if c + w > max_col {
                break;
            }
            self.set(r, c, ch, style);
            if w == 2 {
                self.set(r, c + 1, '\0', style);
            }
            c += w;
        }
        c
    }

    /// Copy `src` into this frame with its top-left corner at (row0, col0); what does not fit is cut.
    pub fn blit(&mut self, src: &Frame, row0: usize, col0: usize) {
        for r in 0..src.rows {
            let dr = r + row0;
            if dr >= self.rows {
                break;
            }
            for c in 0..src.cols {
                let dc = c + col0;
                if dc >= self.cols {
                    break;
                }
                self.cells[dr * self.cols + dc] = src.cells[r * src.cols + c];
            }
        }
    }

    pub fn fill_row(&mut self, r: usize, c0: usize, c1: usize, style: Style) {
        for c in c0..c1.min(self.cols) {
            self.set(r, c, ' ', style);
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

    /// Whole frame as plain text (used by tests and `:dump`).
    pub fn text(&self) -> String {
        (0..self.rows).map(|r| self.row_text(r)).collect::<Vec<_>>().join("\n")
    }
}

pub struct Renderer {
    prev: Vec<Cell>,
    rows: usize,
    cols: usize,
    last_shape: u8,
}

fn push_sgr(out: &mut String, s: &Style) {
    out.push_str("\x1b[0");
    if s.attrs & BOLD != 0 {
        out.push_str(";1");
    }
    if s.attrs & DIM != 0 {
        out.push_str(";2");
    }
    if s.attrs & ITALIC != 0 {
        out.push_str(";3");
    }
    if s.attrs & UNDERLINE != 0 {
        out.push_str(";4");
    }
    if s.attrs & REVERSE != 0 {
        out.push_str(";7");
    }
    if s.attrs & STRIKE != 0 {
        out.push_str(";9");
    }
    match s.fg {
        Color::Default => {}
        Color::Idx(n) if n < 8 => {
            let _ = write!(out, ";{}", 30 + n as u32);
        }
        Color::Idx(n) if n < 16 => {
            let _ = write!(out, ";{}", 90 + (n as u32 - 8));
        }
        Color::Idx(n) => {
            let _ = write!(out, ";38;5;{}", n);
        }
        Color::Rgb(r, g, b) => {
            let _ = write!(out, ";38;2;{};{};{}", r, g, b);
        }
    }
    match s.bg {
        Color::Default => {}
        Color::Idx(n) if n < 8 => {
            let _ = write!(out, ";{}", 40 + n as u32);
        }
        Color::Idx(n) if n < 16 => {
            let _ = write!(out, ";{}", 100 + (n as u32 - 8));
        }
        Color::Idx(n) => {
            let _ = write!(out, ";48;5;{}", n);
        }
        Color::Rgb(r, g, b) => {
            let _ = write!(out, ";48;2;{};{};{}", r, g, b);
        }
    }
    out.push('m');
}

impl Renderer {
    pub fn new() -> Renderer {
        Renderer { prev: Vec::new(), rows: 0, cols: 0, last_shape: 255 }
    }

    /// Force a full repaint on the next render.
    pub fn invalidate(&mut self) {
        self.prev.clear();
        self.rows = 0;
        self.cols = 0;
    }

    pub fn render(&mut self, f: &Frame) -> String {
        let mut out = String::with_capacity(2048);
        out.push_str("\x1b[?2026h\x1b[?25l");
        let resized = self.rows != f.rows || self.cols != f.cols;
        if resized {
            out.push_str("\x1b[0m\x1b[2J");
            self.prev = vec![Cell { ch: '\u{1}', style: Style::DEFAULT }; f.rows * f.cols];
            self.rows = f.rows;
            self.cols = f.cols;
        }
        for r in 0..f.rows {
            let range = r * f.cols..(r + 1) * f.cols;
            let cur = &f.cells[range.clone()];
            if cur == &self.prev[range] {
                continue;
            }
            let _ = write!(out, "\x1b[{};1H\x1b[0m", r + 1);
            let mut style = Style::DEFAULT;
            let mut end = f.cols;
            while end > 0 && cur[end - 1] == Cell::BLANK {
                end -= 1;
            }
            for cell in &cur[..end] {
                if cell.ch == '\0' {
                    continue;
                }
                if cell.style != style {
                    push_sgr(&mut out, &cell.style);
                    style = cell.style;
                }
                out.push(printable(cell.ch));
            }
            if end < f.cols {
                if style != Style::DEFAULT {
                    out.push_str("\x1b[0m");
                }
                out.push_str("\x1b[K");
            }
        }
        self.prev.clone_from(&f.cells);
        out.push_str("\x1b[0m");
        if f.cursor_shape != self.last_shape {
            let n = match f.cursor_shape {
                1 => 6,
                2 => 4,
                _ => 2,
            };
            let _ = write!(out, "\x1b[{} q", n);
            self.last_shape = f.cursor_shape;
        }
        if let Some((r, c)) = f.cursor {
            let _ = write!(out, "\x1b[{};{}H\x1b[?25h", r + 1, c + 1);
        }
        out.push_str("\x1b[?2026l");
        out
    }
}

impl Default for Renderer {
    fn default() -> Renderer {
        Renderer::new()
    }
}
