//! The top bar (the ≡ button and one tab per buffer), the project panel on the left (a file tree,
//! see `tree.rs`), their keys, and the mouse.

use crate::editor::{idx_at_display, Doc, Editor, Focus, Mode};
use crate::input::{Key, Mouse};
use crate::render::THEME_BG;
use crate::screen::{char_width, Color, Frame, Style, BOLD};
use crate::tree::{Git, Kind};

/// The button occupies the first columns of the top bar.
pub const BUTTON_W: usize = 3;
pub const MIN_SIDEBAR: usize = 16;
pub const MAX_SIDEBAR: usize = 80;
/// The editing area is never narrower than this: on a narrower terminal the panel gives way.
const MIN_MAIN_COLS: usize = 30;
/// Lines the mouse wheel moves.
const WHEEL_LINES: isize = 3;

const BAR_BG: Color = Color::Idx(235);
const PANEL_BG: Color = Color::Idx(235);
const SEL_FOCUSED: Color = Color::Idx(24);
const SEL_BLURRED: Color = Color::Idx(238);
const RULE: Color = Color::Idx(238);
/// The panel's edge while it is being dragged.
const RULE_DRAG: Color = Color::Idx(75);
/// The close button of a tab.
const CLOSE: char = '\u{d7}';

/// What is under a mouse position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Button,
    Tab(usize),
    /// the x of a tab
    TabClose(usize),
    /// the panel's right edge: where it is dragged
    Edge,
    Tree(usize),
    TreeBlank,
    /// a cell of the text area (0-based, relative to the editing area)
    Text { row: usize, col: usize },
    /// the terminal pane (relative to the pane); `header` is its title bar
    Term { row: usize, col: usize, header: bool },
    Other,
}

fn width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// `s` cut to at most `max` columns, ending in `…` when something was left out.
pub fn clip(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = char_width(ch);
        if w + cw + 1 > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    if max > 0 {
        out.push('\u{2026}');
    }
    out
}

fn tab_label(d: &Doc) -> String {
    let name = match &d.buf.path {
        Some(p) => p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.to_string_lossy().to_string()),
        None => "[No Name]".to_string(),
    };
    format!(" {}{} {} ", clip(&name, 28), if d.buf.modified { " \u{25cf}" } else { "" }, CLOSE)
}

fn git_color(g: Git) -> Option<Color> {
    match g {
        Git::Clean => None,
        Git::Ignored => Some(Color::Idx(241)),
        Git::Added => Some(Color::Idx(114)),
        Git::Modified => Some(Color::Idx(179)),
        Git::Deleted => Some(Color::Idx(167)),
        Git::Conflict => Some(Color::Idx(203)),
    }
}

impl Editor {
    // ---- geometry ----------------------------------------------------------------------------

    /// Rows of the top bar: one, unless the screen is too small for it.
    pub fn bar_rows(&self) -> usize {
        if self.rows >= 6 {
            1
        } else {
            0
        }
    }

    /// Columns taken by the project panel including its right edge; 0 when it is hidden or the
    /// terminal is too narrow to share.
    pub fn sidebar_cols(&self) -> usize {
        if !self.opts.sidebar || self.bar_rows() == 0 {
            return 0;
        }
        // the wanted width, made smaller where the editing area would get too narrow; the panel
        // goes away only when there is no room for even its narrowest form
        let w = self.opts.sidebar_width.clamp(MIN_SIDEBAR, MAX_SIDEBAR).min(self.cols.saturating_sub(MIN_MAIN_COLS));
        if w >= MIN_SIDEBAR {
            w
        } else {
            0
        }
    }

    /// The widest the panel may be made on this terminal.
    pub fn max_sidebar_cols(&self) -> usize {
        MAX_SIDEBAR.min(self.cols.saturating_sub(MIN_MAIN_COLS)).max(MIN_SIDEBAR)
    }

    /// Rows and columns left for the editing area.
    pub fn main_rows(&self) -> usize {
        self.rows.saturating_sub(self.bar_rows())
    }

    pub fn main_cols(&self) -> usize {
        self.cols.saturating_sub(self.sidebar_cols()).max(1)
    }

    // ---- state -------------------------------------------------------------------------------

    /// Called before every frame: lists the tree the first time it is needed, and lets the panel
    /// follow the file being edited.
    pub fn tree_sync(&mut self) {
        if !self.opts.sidebar {
            return;
        }
        if !self.tree.loaded {
            self.tree.load();
        }
        let cur = self.docs.get(self.cur).and_then(|d| d.buf.path.clone());
        if cur != self.tree.revealed_for {
            self.tree.current = cur.as_ref().and_then(|p| std::fs::canonicalize(p).ok());
            if let Some(p) = self.tree.current.clone() {
                self.tree.reveal(&p);
            }
            self.tree.revealed_for = cur;
        }
        if self.tree.focused && self.sidebar_cols() == 0 {
            self.tree.focused = false;
        }
    }

    /// Picks up what the git thread found.
    pub fn tree_tick(&mut self) {
        self.tree.poll();
    }

    /// Lists the open directories again and asks git again (after a file was written).
    pub fn tree_refresh(&mut self) {
        if self.opts.sidebar && self.tree.loaded {
            self.tree.rebuild();
            self.tree.request_git();
        }
    }

    /// The working directory changed: the panel starts over at the new one.
    pub fn tree_new_root(&mut self) {
        self.tree.reset();
        self.tree.focused = false;
    }

    /// Alt-b, `:tree`: show the panel and give it the keyboard, or hide it.
    pub fn toggle_sidebar(&mut self) {
        if self.opts.sidebar {
            self.opts.sidebar = false;
            self.tree.focused = false;
        } else {
            self.opts.sidebar = true;
            self.focus_sidebar();
        }
    }

    /// Ctrl-w h: show the panel if need be and give it the keyboard.
    pub fn focus_sidebar(&mut self) {
        self.opts.sidebar = true;
        if self.sidebar_cols() == 0 {
            self.set_msg("the window is too small for the project panel");
            return;
        }
        self.tree_sync();
        self.completion = None;
        self.snippet = None;
        self.tree.pending = None;
        self.tree.focused = true;
        self.focus = Focus::Editor;
        self.tree.request_git();
    }

    pub fn unfocus_sidebar(&mut self) {
        self.tree.focused = false;
        self.tree.pending = None;
    }

    // ---- keys --------------------------------------------------------------------------------

    /// A key while the panel has the keyboard.
    pub fn tree_key(&mut self, k: Key) {
        let pending = self.tree.pending.take();
        let half = (self.tree.view_h / 2).max(1) as isize;
        if pending == Some('w') {
            // Ctrl-w l (or Ctrl-w Ctrl-l): back to the editor; Ctrl-w h: already here
            if matches!(k, Key::Char('l') | Key::Ctrl('l') | Key::Char('w') | Key::Ctrl('w')) {
                self.unfocus_sidebar();
            }
            return;
        }
        match k {
            Key::Esc | Key::Char('q') => self.unfocus_sidebar(),
            Key::Ctrl('w') => self.tree.pending = Some('w'),
            Key::Char('j') | Key::Down | Key::Ctrl('n') => self.tree.move_sel(1),
            Key::Char('k') | Key::Up | Key::Ctrl('p') => self.tree.move_sel(-1),
            Key::Char('g') => {
                if pending == Some('g') {
                    self.tree.select(0);
                } else {
                    self.tree.pending = Some('g');
                }
            }
            Key::Home => self.tree.select(0),
            Key::Char('G') | Key::End => {
                let last = self.tree.rows.len().saturating_sub(1);
                self.tree.select(last);
            }
            Key::Ctrl('d') | Key::PageDown => self.tree.move_sel(half),
            Key::Ctrl('u') | Key::PageUp => self.tree.move_sel(-half),
            Key::Enter | Key::Char('o') | Key::Char(' ') => {
                let i = self.tree.sel;
                self.tree_open(i);
            }
            Key::Char('l') | Key::Right => {
                let i = self.tree.sel;
                match self.tree.rows.get(i) {
                    Some(r) if r.kind == Kind::Dir && !r.open => self.tree.expand(i),
                    Some(r) if r.kind == Kind::Dir => {
                        // already open: step into it
                        if self.tree.rows.get(i + 1).is_some_and(|c| c.depth > r.depth) {
                            self.tree.move_sel(1);
                        }
                    }
                    Some(_) => self.tree_open(i),
                    None => {}
                }
            }
            Key::Char('h') | Key::Left => {
                let i = self.tree.sel;
                match self.tree.rows.get(i) {
                    Some(r) if r.kind == Kind::Dir && r.open => self.tree.collapse(i),
                    Some(_) => {
                        if let Some(p) = self.tree.parent_of(i) {
                            self.tree.select(p);
                        }
                    }
                    None => {}
                }
            }
            // new file, new folder, delete (a folder needs `dd`)
            Key::Char('a') => self.panel_new(false),
            Key::Char('f') => self.panel_new(true),
            Key::Char('d') => self.panel_delete(pending == Some('d')),
            Key::Char('R') | Key::Ctrl('r') => {
                self.tree.rebuild();
                self.tree.request_git();
                self.set_msg("project panel refreshed");
            }
            Key::Char(':') => {
                // the command line is the editor's: hand the key over
                self.unfocus_sidebar();
                self.handle_key(Key::Char(':'));
            }
            _ => {}
        }
    }

    /// Enter on a row: open or close a directory, open a file (and give the editor the keyboard).
    fn tree_open(&mut self, i: usize) {
        let Some(row) = self.tree.rows.get(i).cloned() else { return };
        match row.kind {
            Kind::Dir => self.tree.toggle(i),
            Kind::File | Kind::Link => match std::fs::metadata(&row.path) {
                // what the name leads to *now*: a file that went away, a link that leads nowhere or
                // to a directory, a pipe or a device is not opened (that would make an empty new
                // buffer for the path, or block on the pipe)
                Ok(m) if m.is_file() => match self.open_file(&row.path) {
                    Ok(idx) => {
                        self.switch_to(idx);
                        self.unfocus_sidebar();
                    }
                    Err(e) => self.set_err(e),
                },
                Ok(m) if m.is_dir() => self.set_err(format!("{}: a link to a directory (links are not followed)", row.name)),
                Ok(_) => self.set_err(format!("{}: not a regular file", row.name)),
                Err(e) => self.set_err(format!("{}: {} (R refreshes the list)", row.name, e)),
            },
            Kind::Other => self.set_err(format!("{}: not a regular file", row.name)),
            Kind::More => self.set_msg("more names than are listed: Ctrl-p finds a file by name"),
        }
    }

    // ---- drawing -----------------------------------------------------------------------------

    /// Where the tabs go: (buffer, first column, column after the last), for a bar `total` columns wide
    /// whose tabs start at column `x0`. The current buffer's tab is always on screen.
    pub fn tab_layout(&self, x0: usize, total: usize) -> Vec<(usize, usize, usize)> {
        let labels: Vec<usize> = self.docs.iter().map(|d| width(&tab_label(d))).collect();
        if labels.is_empty() {
            return Vec::new();
        }
        let avail = total.saturating_sub(x0);
        let cur = self.cur.min(labels.len() - 1);
        let mut first = 0;
        let mut sum: usize = labels[..=cur].iter().sum();
        while first < cur && sum > avail {
            sum -= labels[first];
            first += 1;
        }
        let mut out = Vec::new();
        let mut x = x0;
        for (i, full) in labels.iter().enumerate().skip(first) {
            if x >= total || (x + full > total && i != cur) {
                break;
            }
            let w = (*full).min(total - x);
            out.push((i, x, x + w));
            x += w;
        }
        out
    }

    /// The column of the x of the tab that occupies columns `a..b`, if it is on screen.
    fn tab_close_col(&self, idx: usize, a: usize, b: usize) -> Option<usize> {
        let w = width(&tab_label(&self.docs[idx]));
        let x = a + w.checked_sub(2)?;
        (x < b).then_some(x)
    }

    pub fn draw_bar(&self, f: &mut Frame) {
        if self.bar_rows() == 0 || f.rows == 0 {
            return;
        }
        let cols = f.cols;
        let base = Style::DEFAULT.with_bg(BAR_BG).with_fg(Color::Idx(250));
        f.fill_row(0, 0, cols, base);
        f.put_str(0, 1, "\u{2261}", base.with_fg(Color::Idx(252)).with(BOLD), cols);
        let sw = self.sidebar_cols();
        let x0 = if sw > 0 {
            let name = clip(&self.tree.root_name(), sw.saturating_sub(BUTTON_W + 1));
            f.put_str(0, BUTTON_W, &name, base.with(BOLD), sw - 1);
            f.set(0, sw - 1, '\u{2502}', Style::fg(self.rule_colour()).with_bg(BAR_BG));
            sw
        } else {
            BUTTON_W
        };
        for (idx, a, b) in self.tab_layout(x0, cols) {
            let current = idx == self.cur;
            let style = if current { Style::DEFAULT.with_bg(THEME_BG).with_fg(Color::Idx(252)).with(BOLD) } else { base.with_fg(Color::Idx(245)) };
            f.fill_row(0, a, b, style);
            let d = &self.docs[idx];
            let label = tab_label(d);
            let end = f.put_str(0, a, &label, style, b);
            if let Some(x) = self.tab_close_col(idx, a, b) {
                f.set(0, x, CLOSE, style.with_fg(if current { Color::Idx(250) } else { Color::Idx(243) }));
            }
            if d.buf.modified {
                // the dot keeps its own colour
                for x in a..end {
                    if f.get(0, x).ch == '\u{25cf}' {
                        f.set(0, x, '\u{25cf}', style.with_fg(Color::Idx(179)));
                    }
                }
            }
        }
    }

    fn rule_colour(&self) -> Color {
        if self.sidebar_drag {
            RULE_DRAG
        } else {
            RULE
        }
    }

    pub fn draw_sidebar(&mut self, f: &mut Frame) {
        let sw = self.sidebar_cols();
        if sw == 0 {
            return;
        }
        let bar = self.bar_rows();
        let h = f.rows.saturating_sub(bar);
        self.tree.set_view(h);
        let cw = sw - 1;
        let panel = Style::DEFAULT.with_bg(PANEL_BG).with_fg(Color::Idx(250));
        let focused = self.tree.focused;
        for i in 0..h {
            let y = bar + i;
            f.fill_row(y, 0, cw, panel);
            f.set(y, cw, '\u{2502}', Style::fg(self.rule_colour()).with_bg(PANEL_BG));
            let Some(row) = self.tree.rows.get(self.tree.top + i) else { continue };
            let selected = self.tree.top + i == self.tree.sel;
            let mut style = panel;
            if selected {
                style = style.with_bg(if focused { SEL_FOCUSED } else { SEL_BLURRED });
                f.fill_row(y, 0, cw, style);
            }
            let x = 1 + row.depth * 2;
            if x + 2 >= cw {
                continue;
            }
            let is_current = self.tree.current.as_deref().is_some_and(|c| c == row.path.as_path());
            let name_style = match row.kind {
                Kind::More => style.with_fg(Color::Idx(243)),
                _ => {
                    let base = match git_color(row.git) {
                        Some(c) => style.with_fg(c),
                        None if row.kind == Kind::Dir => style.with_fg(Color::Idx(252)),
                        None => style,
                    };
                    if is_current {
                        base.with(BOLD)
                    } else {
                        base
                    }
                }
            };
            if row.kind == Kind::Dir {
                let chev = if row.open { "\u{25be}" } else { "\u{25b8}" };
                f.put_str(y, x, chev, style.with_fg(Color::Idx(245)), cw);
            }
            let mut name = row.name.clone();
            if row.kind == Kind::Link {
                name.push_str(" \u{2192}");
            }
            let shown = clip(&name, cw.saturating_sub(x + 2));
            f.put_str(y, x + 2, &shown, name_style, cw);
        }
        if self.tree.rows.is_empty() && h > 0 {
            f.put_str(bar, 2, "(empty)", Style::fg(Color::Idx(243)).with_bg(PANEL_BG), cw);
        }
    }

    // ---- mouse -------------------------------------------------------------------------------

    pub fn hit_test(&mut self, y: usize, x: usize) -> Hit {
        let bar = self.bar_rows();
        let sw = self.sidebar_cols();
        if y < bar {
            if x < BUTTON_W {
                return Hit::Button;
            }
            if sw > 0 && x + 1 == sw {
                return Hit::Edge;
            }
            let x0 = if sw > 0 { sw } else { BUTTON_W };
            let Some((i, a, b)) = self.tab_layout(x0, self.cols).into_iter().find(|&(_, a, b)| x >= a && x < b) else { return Hit::Other };
            // the x and the space on each side of it
            return match self.tab_close_col(i, a, b) {
                Some(c) if x + 1 >= c && x <= c + 1 => Hit::TabClose(i),
                _ => Hit::Tab(i),
            };
        }
        let my = y - bar;
        if x < sw {
            if x + 1 == sw {
                return Hit::Edge;
            }
            let i = self.tree.top.saturating_add(my);
            return if i < self.tree.rows.len() { Hit::Tree(i) } else { Hit::TreeBlank };
        }
        let mx = x - sw;
        let lay = self.layout();
        if my < lay.text_rows {
            return Hit::Text { row: my, col: mx };
        }
        if let Some(h) = lay.term_header {
            if my == h {
                return Hit::Term { row: 0, col: mx, header: true };
            }
            if my >= lay.term_top && my < lay.term_top + lay.term_rows {
                return Hit::Term { row: my - lay.term_top, col: mx, header: false };
            }
        }
        Hit::Other
    }

    /// Hand a mouse event to the program in the terminal pane, if it asked for them.
    fn forward_mouse(&mut self, m: Mouse, row: usize, col: usize) -> bool {
        let Some(t) = self.term.as_mut() else { return false };
        if !(t.vt.mouse && t.vt.mouse_sgr) {
            return false;
        }
        // a program that asked for clicks only (mode 1000) is not sent the mouse moving
        if m.motion() && !t.vt.mouse_motion {
            return false;
        }
        let (r, c) = (row.min(t.vt.rows.saturating_sub(1)), col.min(t.vt.cols.saturating_sub(1)));
        let seq = format!("\x1b[<{};{};{}{}", m.button, c + 1, r + 1, if m.release { 'm' } else { 'M' });
        t.pty.write_all(seq.as_bytes());
        true
    }

    /// The panel's edge follows the pointer: the width is what puts the edge under column `x`.
    fn drag_sidebar_to(&mut self, x: usize) {
        self.opts.sidebar_width = x.saturating_add(1).clamp(MIN_SIDEBAR, self.max_sidebar_cols());
    }

    /// Close the tab of buffer `i`. One with unsaved changes asks for a second click first.
    fn close_tab(&mut self, i: usize, confirmed: bool) {
        let Some(d) = self.docs.get(i) else { return };
        if d.buf.modified && !confirmed {
            let msg = format!("{} has unsaved changes: click x again to discard them (or save with :w)", d.name());
            self.confirm_close = Some(i);
            self.set_err(msg);
            return;
        }
        self.unfocus_sidebar();
        self.focus = Focus::Editor;
        if let Err(e) = self.delete_buffer_at(i, true) {
            self.set_err(e);
        }
    }

    pub fn handle_mouse(&mut self, m: Mouse) {
        if !self.opts.mouse {
            self.sidebar_drag = false;
            return;
        }
        let (y, x) = (m.row as usize, m.col as usize);
        // dragging the panel's edge: the left button is down, so what moves or releases it is ours
        if self.sidebar_drag {
            if m.is_wheel() {
                return;
            }
            if m.button & 3 == 0 && (m.motion() || m.release) && self.sidebar_cols() > 0 {
                self.drag_sidebar_to(x);
                if m.release {
                    self.sidebar_drag = false;
                    let w = self.opts.sidebar_width;
                    self.set_msg(format!("panel width {} (`set sidebarwidth={}` in your config keeps it)", w, w));
                }
                return;
            }
            // anything else means the release was lost: the drag is over
            self.sidebar_drag = false;
            return;
        }
        // a "click again" is answered by the next press, wherever it lands
        let confirm = if m.is_wheel() || m.motion() || m.release { self.confirm_close } else { self.confirm_close.take() };
        // a command line, a picker or a popup has the screen: the wheel scrolls a popup, nothing else acts
        if matches!(self.mode, Mode::Command | Mode::Search) || self.picker.is_some() || self.popup.is_some() || self.prompt.is_some() {
            if let Some(p) = self.popup.as_mut() {
                if m.wheel_down() {
                    p.scroll += WHEEL_LINES as usize;
                } else if m.wheel_up() {
                    p.scroll = p.scroll.saturating_sub(WHEEL_LINES as usize);
                }
            }
            return;
        }
        let hit = self.hit_test(y, x);
        if m.is_wheel() {
            self.mouse_wheel(m, hit);
            return;
        }
        // a click that the terminal program asked for belongs to it (press and release both)
        if let Hit::Term { row, col, header: false } = hit {
            if (m.release || m.motion()) && self.focus != Focus::Editor && self.forward_mouse(m, row, col) {
                return;
            }
        }
        if m.middle_press() {
            if let Hit::Tab(i) | Hit::TabClose(i) = hit {
                self.close_tab(i, confirm == Some(i));
            }
            return;
        }
        if !m.left_press() {
            return;
        }
        match hit {
            Hit::Edge => {
                if self.sidebar_cols() > 0 {
                    self.sidebar_drag = true;
                }
            }
            Hit::TabClose(i) => self.close_tab(i, confirm == Some(i)),
            Hit::Button => {
                self.opts.sidebar = !self.opts.sidebar;
                if self.opts.sidebar {
                    self.tree_sync();
                } else {
                    self.tree.focused = false;
                }
            }
            Hit::Tab(i) => {
                self.unfocus_sidebar();
                self.focus = Focus::Editor;
                self.switch_to(i);
            }
            Hit::Tree(i) => {
                self.tree.focused = true;
                self.tree.pending = None;
                self.focus = Focus::Editor;
                self.completion = None;
                self.tree.select(i);
                self.tree_open(i);
            }
            Hit::TreeBlank => {
                self.tree.focused = true;
                self.focus = Focus::Editor;
            }
            Hit::Text { row, col } => self.click_text(row, col),
            Hit::Term { row, col, header } => {
                if self.term_visible() {
                    if !header {
                        self.forward_mouse(m, row, col);
                    }
                    self.tree.focused = false;
                    if self.focus == Focus::Editor {
                        self.completion = None;
                        self.snippet = None;
                        self.focus = Focus::TermInsert;
                    }
                }
            }
            Hit::Other => {}
        }
    }

    /// A click in the text: the cursor goes there (in insert mode too: it just moves, like an arrow key).
    fn click_text(&mut self, row: usize, col: usize) {
        self.unfocus_sidebar();
        self.focus = Focus::Editor;
        self.completion = None;
        self.snippet = None;
        let gw = self.gutter_width();
        let ts = self.opts.tabstop;
        let d = &self.docs[self.cur];
        let r = d.top.saturating_add(row).min(d.buf.line_count().saturating_sub(1));
        let idx = if col < gw { 0 } else { idx_at_display(&d.buf.lines[r], d.left.saturating_add(col - gw), ts) };
        self.move_to((r, idx));
    }

    fn mouse_wheel(&mut self, m: Mouse, hit: Hit) {
        let delta = if m.wheel_up() {
            -WHEEL_LINES
        } else if m.wheel_down() {
            WHEEL_LINES
        } else {
            return;
        };
        match hit {
            Hit::Tree(_) | Hit::TreeBlank => self.tree.scroll(delta),
            Hit::Text { .. } => self.scroll_text(delta),
            Hit::Term { row, col, .. } => {
                if self.forward_mouse(m, row, col) {
                    return;
                }
                let Some(t) = self.term.as_mut() else { return };
                if t.vt.in_alt {
                    return;
                }
                let max = t.vt.scrollback.len() as isize;
                t.scroll = (t.scroll as isize - delta).clamp(0, max) as usize;
                let scrolled = t.scroll > 0;
                if scrolled && self.focus == Focus::TermInsert {
                    self.focus = Focus::TermNormal;
                } else if !scrolled && self.focus == Focus::TermNormal {
                    self.focus = Focus::TermInsert;
                }
            }
            _ => {}
        }
    }

    /// Scroll the text window by `delta` lines; the cursor stays on screen (inside `scrolloff`).
    fn scroll_text(&mut self, delta: isize) {
        let h = self.view_h.max(1);
        let ts = self.opts.tabstop;
        let so = self.opts.scrolloff.min(h.saturating_sub(1) / 2);
        let d = &mut self.docs[self.cur];
        let n = d.buf.line_count();
        let top = (d.top as isize + delta).clamp(0, n as isize - 1) as usize;
        d.top = top;
        let lo = if top == 0 { 0 } else { top + so };
        let hi = if top + h >= n { n - 1 } else { top + h - 1 - so };
        let row = d.cursor.0.clamp(lo.min(hi), hi).min(n - 1);
        if row != d.cursor.0 {
            let col = idx_at_display(&d.buf.lines[row], d.want, ts);
            d.cursor = (row, col);
            self.clamp_cursor();
        }
    }
}
