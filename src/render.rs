//! Turns editor state into a `Frame`.

use std::collections::HashMap;

use crate::config::Theme;
use crate::editor::{cell_width, display_col, find_in_line_fwd, Editor, Focus, Mode};
use crate::highlight;
use crate::screen::{char_width, Color, Frame, Style, BOLD, DIM, ITALIC, REVERSE, UNDERLINE};

/// Replace the terminal's default background / foreground by the dark theme's.
pub fn apply_dark(f: &mut Frame) {
    for c in f.cells.iter_mut() {
        if c.style.bg == Color::Default {
            c.style.bg = THEME_BG;
        }
        if c.style.fg == Color::Default {
            c.style.fg = THEME_FG;
        }
    }
}

pub struct Layout {
    pub text_rows: usize,
    pub status_row: usize,
    pub term_header: Option<usize>,
    pub term_top: usize,
    pub term_rows: usize,
    pub cmd_row: usize,
}

// palette (256-colour indices so it works on any modern terminal)
const C_DIM: Color = Color::Idx(243);
const C_LINENR_CUR: Color = Color::Idx(250);
const C_ERR: Color = Color::Idx(203);
/// Indent guides, and the one of the block the cursor is in.
const C_GUIDE: Color = Color::Idx(238);
const C_GUIDE_SCOPE: Color = Color::Idx(245);
const C_WARN: Color = Color::Idx(179);
const C_INFO: Color = Color::Idx(75);
const C_HINT: Color = Color::Idx(109);
/// Background and foreground of the dark theme (256-colour greys: the terminal's own palette cannot change them).
pub const THEME_BG: Color = Color::Idx(234);
pub const THEME_FG: Color = Color::Idx(252);
/// `THEME_BG` as RGB (xterm's grey ramp: 232 + n is 8 + 10 * n), for setting the terminal's own background.
pub const THEME_BG_RGB: (u8, u8, u8) = (0x1c, 0x1c, 0x1c);
pub const BG_SEL: Color = Color::Idx(238);
const BG_BAR: Color = Color::Idx(237);
const BG_POP: Color = Color::Idx(236);
const BG_POP_SEL: Color = Color::Idx(24);

fn syn_style(class: u8) -> Style {
    match class {
        highlight::KEYWORD => Style::fg(Color::Idx(176)),
        highlight::STRING => Style::fg(Color::Idx(114)),
        highlight::COMMENT => Style::fg(Color::Idx(244)).with(ITALIC),
        highlight::NUMBER => Style::fg(Color::Idx(209)),
        highlight::TYPE => Style::fg(Color::Idx(80)),
        highlight::FUNC => Style::fg(Color::Idx(111)),
        highlight::MACRO => Style::fg(Color::Idx(141)),
        highlight::ATTR => Style::fg(Color::Idx(137)),
        highlight::HEADING => Style::fg(Color::Idx(179)).with(BOLD),
        _ => Style::DEFAULT,
    }
}

pub fn sev_color(sev: u8) -> Color {
    match sev {
        1 => C_ERR,
        2 => C_WARN,
        3 => C_INFO,
        _ => C_HINT,
    }
}

pub fn wrap_text(s: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out = Vec::new();
    for raw in s.split('\n') {
        let raw = raw.trim_end();
        if raw.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        let mut w = 0;
        for word in raw.split_inclusive(' ') {
            let ww: usize = word.chars().map(char_width).sum();
            if w + ww > width && w > 0 {
                out.push(cur.trim_end().to_string());
                cur.clear();
                w = 0;
            }
            if ww > width {
                for ch in word.chars() {
                    let cw = char_width(ch);
                    if w + cw > width {
                        out.push(std::mem::take(&mut cur));
                        w = 0;
                    }
                    cur.push(ch);
                    w += cw;
                }
            } else {
                cur.push_str(word);
                w += ww;
            }
        }
        out.push(cur.trim_end().to_string());
    }
    out
}

impl Editor {
    pub fn term_rows_wanted(&self) -> usize {
        let rows = self.main_rows();
        if !self.term_visible() || rows < 12 {
            return 0;
        }
        let want = rows * self.opts.term_height.clamp(10, 80) / 100;
        want.clamp(4, rows.saturating_sub(8).max(4))
    }

    pub fn layout(&mut self) -> Layout {
        let tr = self.term_rows_wanted();
        let rows = self.main_rows().max(3);
        let (text_rows, term_header, term_top) = if tr > 0 {
            let text_rows = rows - 3 - tr;
            (text_rows, Some(text_rows + 1), text_rows + 2)
        } else {
            (rows - 2, None, 0)
        };
        let gw = self.gutter_width();
        self.view_h = text_rows.max(1);
        self.view_w = self.main_cols().saturating_sub(gw).max(1);
        Layout { text_rows, status_row: text_rows, term_header, term_top, term_rows: tr, cmd_row: rows - 1 }
    }

    pub fn gutter_width(&self) -> usize {
        let mut w = 1; // sign column
        if self.opts.number || self.opts.relativenumber {
            let digits = self.buf().line_count().to_string().len().max(3);
            w += digits + 1;
        }
        w
    }

    /// Scroll so the cursor is visible, honouring `scrolloff`.
    pub fn adjust_view(&mut self) {
        let h = self.view_h.max(1);
        let w = self.view_w.max(1);
        let so = self.opts.scrolloff.min(h.saturating_sub(1) / 2);
        let ts = self.opts.tabstop;
        let d = &mut self.docs[self.cur];
        let n = d.buf.line_count();
        let last = n - 1;
        d.top = d.top.min(last);
        let row = d.cursor.0.min(last);
        if row < d.top + so {
            d.top = row.saturating_sub(so);
        }
        let bottom_need = (row + so).min(last);
        if bottom_need >= d.top + h {
            d.top = bottom_need + 1 - h;
        }
        // horizontal
        let line = &d.buf.lines[row];
        let dc = display_col(line, d.cursor.1.min(line.len()), ts);
        let cw = line.get(d.cursor.1).map_or(1, |c| cell_width(*c, dc, ts));
        if dc < d.left {
            d.left = dc.saturating_sub(w / 4);
        } else if dc + cw > d.left + w {
            d.left = dc + cw - w + w / 4;
        }
        if d.left > 0 && dc + cw <= w {
            d.left = 0;
        }
    }

    fn in_selection(&self, r: usize, c: usize) -> bool {
        let (a, b) = (self.visual_anchor, self.cursor());
        let (s, e) = if a <= b { (a, b) } else { (b, a) };
        match self.mode {
            Mode::VisualLine => r >= s.0 && r <= e.0,
            Mode::Visual => (r, c) >= s && (r, c) <= e,
            _ => false,
        }
    }

    /// The whole screen: the top bar, the project panel and, to the right of it and below the bar,
    /// the editing area (text, status line, terminal pane, command line), which is drawn into a frame
    /// of its own so that all of its coordinates start at zero.
    pub fn draw(&mut self, f: &mut Frame) {
        f.clear();
        self.tree_sync();
        let bar = self.bar_rows();
        let sw = self.sidebar_cols();
        let mut main = Frame::new(f.rows.saturating_sub(bar).max(1), f.cols.saturating_sub(sw).max(1));
        self.draw_main(&mut main);
        f.blit(&main, bar, sw);
        f.cursor = main.cursor.map(|(r, c)| (r + bar, c + sw));
        f.cursor_shape = main.cursor_shape;
        self.draw_bar(f);
        self.draw_sidebar(f);
        // the dark theme: every cell that would have used the terminal's own colours gets ours
        if self.opts.theme == Theme::Dark {
            apply_dark(f);
        }
    }

    fn draw_main(&mut self, f: &mut Frame) {
        f.clear();
        let lay = self.layout();
        self.adjust_view();
        let cols = f.cols;
        let ts = self.opts.tabstop;
        let gw = self.gutter_width();
        let text_w = cols.saturating_sub(gw);
        let num_w = gw - 1;

        // ---- text area ----
        let (top, left) = (self.doc().top, self.doc().left);
        let cursor = self.cursor();
        let n_lines = self.buf().line_count();
        let lang = highlight::lang_for(self.buf().path.as_deref());
        let version = self.buf().version;
        let mut state = match lang {
            Some(l) => {
                let d = &mut self.docs[self.cur];
                d.hl.state_at(l, version, &d.buf.lines, top)
            }
            None => 0,
        };
        // diagnostics by row
        let mut sev_rows: HashMap<usize, u8> = HashMap::new();
        for dg in &self.doc().diags {
            if dg.start.0 >= top && dg.start.0 < top + lay.text_rows + 1 {
                let e = sev_rows.entry(dg.start.0).or_insert(dg.severity);
                if dg.severity < *e {
                    *e = dg.severity;
                }
            }
        }
        let hl_pat: Option<(Vec<char>, bool)> = if self.hl_search && self.opts.hlsearch {
            self.last_search.as_ref().filter(|s| !s.pat.is_empty()).map(|s| (s.pat.clone(), self.search_icase(&s.pat)))
        } else {
            None
        };
        let snip_sel = self.snippet_selection();
        // indent guides: the indentation of every row on screen (blank rows take it from their
        // neighbours) and the block the cursor is in
        let shown_rows = lay.text_rows.min(n_lines.saturating_sub(top));
        // (one level is what the file itself uses, when that is known, otherwise what typing would use)
        let guide_unit = if self.opts.indent_guides {
            let (tabs, sw) = match self.buf().indent {
                Some(i) => (i.use_tabs, i.width),
                None => self.indent_style(),
            };
            Some(if tabs { ts } else { sw }.max(1))
        } else {
            None
        };
        let guide_inds: Vec<usize> = if guide_unit.is_some() { crate::guides::row_indents(&self.buf().lines, top, shown_rows, ts) } else { Vec::new() };
        let guide_scope = guide_unit.and_then(|u| crate::guides::scope(&guide_inds, cursor.0.wrapping_sub(top), u));
        let mut classes: Vec<u8> = Vec::new();
        for sr in 0..lay.text_rows {
            let r = top + sr;
            if r >= n_lines {
                f.put_str(sr, 0, "~", Style::fg(Color::Idx(239)), cols);
                continue;
            }
            // gutter
            if let Some(sev) = sev_rows.get(&r) {
                f.set(sr, 0, '\u{25cf}', Style::fg(sev_color(*sev)));
            }
            if num_w > 0 {
                let is_cur = r == cursor.0;
                let n = if self.opts.relativenumber && !is_cur { r.abs_diff(cursor.0) } else { r + 1 };
                let s = if is_cur && self.opts.relativenumber && self.opts.number { format!("{:<w$} ", n, w = num_w - 1) } else { format!("{:>w$} ", n, w = num_w - 1) };
                let st = if is_cur { Style::fg(C_LINENR_CUR).with(BOLD) } else { Style::fg(C_DIM) };
                f.put_str(sr, 1, &s, st, gw);
            }
            let line = self.buf().lines[r].clone();
            // syntax classes
            if let Some(l) = lang {
                highlight::highlight_line(l, &line, &mut state, &mut classes);
            } else {
                classes.clear();
                classes.resize(line.len(), 0);
            }
            let mut styles: Vec<Style> = classes.iter().map(|c| syn_style(*c)).collect();
            // search matches
            if let Some((pat, ic)) = &hl_pat {
                let mut i = 0;
                while let Some(m) = find_in_line_fwd(&line, pat, i, *ic, false) {
                    for s in styles.iter_mut().skip(m).take(pat.len()) {
                        *s = Style::DEFAULT.with_fg(Color::Idx(0)).with_bg(Color::Idx(179));
                    }
                    i = m + pat.len().max(1);
                }
            }
            // the snippet placeholder that typing will replace
            if let Some((sr_, a, b)) = snip_sel {
                if sr_ == r {
                    for s in styles.iter_mut().take(b).skip(a) {
                        *s = s.with_bg(BG_SEL);
                    }
                }
            }
            // diagnostics underline
            for dg in &self.doc().diags {
                if dg.start.0 <= r && dg.end.0 >= r {
                    let a = if dg.start.0 == r { dg.start.1 } else { 0 };
                    let mut b = if dg.end.0 == r { dg.end.1 } else { line.len() };
                    if b <= a {
                        b = (a + 1).min(line.len().max(a + 1));
                    }
                    for s in styles.iter_mut().take(b).skip(a) {
                        s.attrs |= UNDERLINE;
                    }
                }
            }
            // draw characters
            let visual = self.mode == Mode::Visual || self.mode == Mode::VisualLine;
            let mut dc = 0usize;
            for (i, ch) in line.iter().enumerate() {
                let w = cell_width(*ch, dc, ts);
                if dc >= left + text_w {
                    break;
                }
                let mut st = styles[i];
                if visual && self.in_selection(r, i) {
                    st = st.with_bg(BG_SEL);
                }
                if dc + w > left {
                    let glyphs: Vec<char> = if *ch == '\t' {
                        vec![' '; w]
                    } else if (*ch as u32) < 0x20 || *ch as u32 == 0x7f {
                        vec!['^', if *ch as u32 == 0x7f { '?' } else { (*ch as u8 + 64) as char }]
                    } else if char_width(*ch) == 0 {
                        vec![]
                    } else {
                        vec![*ch]
                    };
                    if *ch == '\t' || (*ch as u32) < 0x20 || *ch as u32 == 0x7f {
                        for (k, g) in glyphs.iter().enumerate() {
                            let x = dc + k;
                            if x >= left && x < left + text_w {
                                let gs = if *ch == '\t' { st.with_fg(Color::Idx(238)) } else { st.with_fg(C_ERR) };
                                f.set(sr, gw + x - left, if *ch == '\t' && k == 0 && false { '\u{2192}' } else { *g }, gs);
                            }
                        }
                    } else if let Some(g) = glyphs.first() {
                        if dc >= left {
                            f.set(sr, gw + dc - left, *g, st);
                            if w == 2 && dc + 1 < left + text_w {
                                f.set(sr, gw + dc - left + 1, '\0', st);
                            } else if w == 2 {
                                f.set(sr, gw + dc - left, ' ', st);
                            }
                        } else {
                            f.set(sr, gw, ' ', st);
                        }
                    }
                }
                dc += w;
            }
            if visual && line.is_empty() && self.in_selection(r, 0) {
                f.set(sr, gw, ' ', Style::DEFAULT.with_bg(BG_SEL));
            }
            if self.mode == Mode::VisualLine && self.in_selection(r, 0) {
                // extend the highlight a little past the text so the line is obviously selected
                let end = (dc.max(left) - left).min(text_w);
                if end < text_w {
                    f.set(sr, gw + end, ' ', Style::DEFAULT.with_bg(BG_SEL));
                }
            }
            // indent guides: only ever over empty cells of the indentation (never over text), keeping
            // whatever background the cell has (a selection, say)
            if let (Some(unit), Some(&ind)) = (guide_unit, guide_inds.get(sr)) {
                let scope_col = guide_scope.filter(|(_, a, b)| (*a..=*b).contains(&sr)).map(|(c, _, _)| c);
                for g in crate::guides::guide_cols(ind, unit, left, left + text_w) {
                    let x = gw + g - left;
                    let cell = f.get(sr, x);
                    if cell.ch == ' ' {
                        let colour = if Some(g) == scope_col { C_GUIDE_SCOPE } else { C_GUIDE };
                        f.set(sr, x, self.opts.indent_char, cell.style.with_fg(colour));
                    }
                }
            }
        }

        // ---- status line ----
        self.draw_status(f, lay.status_row);

        // ---- terminal ----
        if let Some(h) = lay.term_header {
            self.draw_terminal(f, h, lay.term_top, lay.term_rows);
        }

        // ---- command / message line ----
        let cmd_row = lay.cmd_row;
        let mut cursor_pos: Option<(usize, usize)> = None;
        if let Some(p) = &self.prompt {
            let label = if p.is_question() { Style::fg(C_ERR).with(BOLD) } else { Style::fg(Color::Idx(110)).with(BOLD) };
            let mid = f.put_str(cmd_row, 0, &p.label, label, cols);
            let end = f.put_str(cmd_row, mid, &p.input, Style::DEFAULT, cols);
            if !p.is_question() {
                cursor_pos = Some((cmd_row, end.min(cols.saturating_sub(1))));
            }
        } else if self.mode == Mode::Command || self.mode == Mode::Search {
            let s = format!("{}{}", self.cmd_kind, self.cmdline);
            let end = f.put_str(cmd_row, 0, &s, Style::DEFAULT, cols);
            cursor_pos = Some((cmd_row, end.min(cols.saturating_sub(1))));
        } else {
            let mut shown = false;
            if !self.msg.is_empty() {
                let st = if self.msg_err { Style::fg(Color::Idx(231)).with_bg(Color::Idx(124)) } else { Style::DEFAULT };
                let first = self.msg.lines().next().unwrap_or("");
                f.put_str(cmd_row, 0, first, st, cols);
                shown = true;
            } else if let Some(r) = self.recording {
                f.put_str(cmd_row, 0, &format!("recording @{}", r), Style::fg(C_ERR).with(BOLD), cols);
                shown = true;
            } else if let Some((m, sev)) = self.cursor_diag_message() {
                f.put_str(cmd_row, 0, &m, Style::fg(sev_color(sev)), cols);
                shown = true;
            }
            let _ = shown;
            // pending keys on the right
            if !self.keybuf.is_empty() {
                let s: String = self.keybuf.iter().map(key_label).collect();
                let w: usize = s.chars().map(char_width).sum();
                if w < cols {
                    f.put_str(cmd_row, cols - w - 1, &s, Style::fg(C_DIM), cols);
                }
            }
        }

        // ---- cursor ----
        if cursor_pos.is_none() && self.focus == Focus::Editor && !self.tree.focused {
            let (r, c) = cursor;
            if r >= top && r < top + lay.text_rows {
                let dc = display_col(&self.buf().lines[r], c.min(self.buf().lines[r].len()), ts);
                if dc >= left && dc - left < text_w {
                    cursor_pos = Some((r - top, gw + dc - left));
                }
            }
        }
        if self.focus != Focus::Editor && cursor_pos.is_none() && lay.term_rows > 0 {
            cursor_pos = self.term_cursor(lay.term_top, lay.term_rows, cols);
        }

        // ---- overlays ----
        self.draw_completion(f, &lay, gw, left, top);
        if self.popup.is_some() {
            self.draw_popup(f);
            cursor_pos = None;
        }
        if self.picker.is_some() {
            cursor_pos = self.draw_picker(f);
        }

        f.cursor = cursor_pos;
        f.cursor_shape = match self.mode {
            Mode::Insert | Mode::Command | Mode::Search => 1,
            _ => {
                if self.focus == Focus::TermInsert {
                    1
                } else {
                    0
                }
            }
        };
        if self.picker.is_some() {
            f.cursor_shape = 1;
        }
    }

    fn draw_status(&mut self, f: &mut Frame, row: usize) {
        let cols = f.cols;
        let bar = Style::DEFAULT.with_bg(BG_BAR).with_fg(Color::Idx(252));
        f.fill_row(row, 0, cols, bar);
        let (label, bg) = match self.mode {
            _ if self.tree.focused => (" TREE ", Color::Idx(110)),
            Mode::Normal => (" NORMAL ", Color::Idx(75)),
            Mode::Insert => (" INSERT ", Color::Idx(114)),
            Mode::Visual => (" VISUAL ", Color::Idx(176)),
            Mode::VisualLine => (" V-LINE ", Color::Idx(176)),
            Mode::Command | Mode::Search => (" COMMAND ", Color::Idx(180)),
        };
        let chip = Style::DEFAULT.with_bg(bg).with_fg(Color::Idx(0)).with(BOLD);
        let mut x = f.put_str(row, 0, label, chip, cols);
        let d = self.doc();
        let mut name = d.name();
        if d.buf.modified {
            name.push_str(" [+]");
        }
        x = f.put_str(row, x + 1, &name, bar.with(BOLD), cols);
        // right side
        let (e, w) = self.diag_counts();
        let mut right = String::new();
        if let Some(l) = self.lsp_label() {
            right.push_str(&l);
            right.push_str("  ");
        }
        let (r, c) = self.cursor();
        let dc = display_col(&d.buf.lines[r], c.min(d.buf.lines[r].len()), self.opts.tabstop);
        let n = d.buf.line_count();
        let pct = if n <= 1 { 100 } else { r * 100 / (n - 1) };
        let pos = format!("{}:{}  {}%", r + 1, dc + 1, pct);
        let mut diag = String::new();
        if e > 0 {
            diag.push_str(&format!("E{} ", e));
        }
        if w > 0 {
            diag.push_str(&format!("W{} ", w));
        }
        let rw: usize = right.chars().count() + diag.chars().count() + pos.chars().count() + 1;
        if rw + x + 2 < cols {
            let mut px = cols - rw;
            px = f.put_str(row, px, &right, bar.with_fg(Color::Idx(245)), cols);
            if e > 0 {
                px = f.put_str(row, px, &format!("E{} ", e), bar.with_fg(C_ERR).with(BOLD), cols);
            }
            if w > 0 {
                px = f.put_str(row, px, &format!("W{} ", w), bar.with_fg(C_WARN).with(BOLD), cols);
            }
            f.put_str(row, px, &pos, bar, cols);
        }
        let _ = (DIM, REVERSE);
    }

    fn draw_completion(&self, f: &mut Frame, lay: &Layout, gw: usize, left: usize, top: usize) {
        let Some(c) = &self.completion else { return };
        if self.mode != Mode::Insert || c.shown.is_empty() {
            return;
        }
        let (r, col) = self.cursor();
        if r < top || r >= top + lay.text_rows {
            return;
        }
        let ts = self.opts.tabstop;
        let line = &self.buf().lines[r];
        let start_dc = display_col(line, c.start_col.min(line.len()), ts);
        let rows = c.shown.len().min(10);
        let first = if c.sel >= rows { c.sel + 1 - rows } else { 0 };
        let labels: Vec<String> = (first..first + rows)
            .map(|k| {
                let it = &c.items[c.shown[k]];
                let kind = kind_letter(it.kind);
                let det: String = it.detail.chars().take(28).collect();
                if det.is_empty() {
                    format!(" {} {} ", kind, it.label)
                } else {
                    format!(" {} {}  {} ", kind, it.label, det)
                }
            })
            .collect();
        let width = labels.iter().map(|s| s.chars().count()).max().unwrap_or(10).min(f.cols.saturating_sub(4)).max(12);
        let sr = r - top;
        let below = sr + 1 + rows < lay.text_rows + 1 || sr < rows;
        let y0 = if below { sr + 1 } else { sr.saturating_sub(rows) };
        let mut x0 = gw + start_dc.saturating_sub(left);
        if x0 + width > f.cols {
            x0 = f.cols.saturating_sub(width);
        }
        let _ = col;
        for (k, lab) in labels.iter().enumerate() {
            let y = y0 + k;
            if y >= f.rows.saturating_sub(1) {
                break;
            }
            let sel = first + k == c.sel;
            let st = if sel { Style::DEFAULT.with_bg(BG_POP_SEL).with_fg(Color::Idx(255)) } else { Style::DEFAULT.with_bg(BG_POP).with_fg(Color::Idx(250)) };
            f.fill_row(y, x0, x0 + width, st);
            f.put_str(y, x0, lab, st, x0 + width);
        }
    }

    fn draw_popup(&mut self, f: &mut Frame) {
        let Some(p) = &self.popup else { return };
        let cols = f.cols;
        let rows = f.rows;
        if cols < 12 || rows < 6 {
            return; // no room for a box (the keys still work)
        }
        let max_w = cols.saturating_sub(6).clamp(20, 100);
        let mut lines: Vec<String> = Vec::new();
        for l in &p.lines {
            lines.extend(wrap_text(l, max_w - 2));
        }
        let content_w = lines.iter().map(|l| l.chars().map(char_width).sum::<usize>()).max().unwrap_or(10).max(p.title.len() + 2);
        let w = (content_w + 4).min(cols);
        let max_h = rows.saturating_sub(4).max(3);
        let h = (lines.len() + 2).min(max_h);
        let inner = h - 2;
        let scroll = p.scroll.min(lines.len().saturating_sub(inner));
        let x0 = cols.saturating_sub(w) / 2;
        let y0 = (rows.saturating_sub(h)) / 2;
        let bg = Style::DEFAULT.with_bg(BG_POP).with_fg(Color::Idx(252));
        let border = bg.with_fg(Color::Idx(244));
        for y in y0..y0 + h {
            f.fill_row(y, x0, x0 + w, bg);
        }
        // border
        for x in x0 + 1..x0 + w - 1 {
            f.set(y0, x, '\u{2500}', border);
            f.set(y0 + h - 1, x, '\u{2500}', border);
        }
        for y in y0 + 1..y0 + h - 1 {
            f.set(y, x0, '\u{2502}', border);
            f.set(y, x0 + w - 1, '\u{2502}', border);
        }
        f.set(y0, x0, '\u{256d}', border);
        f.set(y0, x0 + w - 1, '\u{256e}', border);
        f.set(y0 + h - 1, x0, '\u{2570}', border);
        f.set(y0 + h - 1, x0 + w - 1, '\u{256f}', border);
        if !p.title.is_empty() {
            f.put_str(y0, x0 + 2, &format!(" {} ", p.title), border.with(BOLD), x0 + w - 1);
        }
        for (i, l) in lines.iter().skip(scroll).take(inner).enumerate() {
            f.put_str(y0 + 1 + i, x0 + 2, l, bg, x0 + w - 2);
        }
        if lines.len() > inner {
            let msg = format!(" {}/{} ", scroll + inner.min(lines.len()), lines.len());
            f.put_str(y0 + h - 1, (x0 + w).saturating_sub(2 + msg.len().min(w.saturating_sub(4))), &msg, border, x0 + w - 1);
        }
        if let Some(p) = self.popup.as_mut() {
            p.scroll = scroll;
        }
    }

    fn draw_picker(&mut self, f: &mut Frame) -> Option<(usize, usize)> {
        let p = self.picker.as_ref()?;
        let cols = f.cols;
        let rows = f.rows;
        if cols < 12 || rows < 6 {
            return None;
        }
        let w = cols.saturating_sub(6).clamp(20, 110).min(cols);
        let max_items = rows.saturating_sub(8).clamp(3, 18);
        let shown = p.items.len().min(max_items);
        let h = shown + 3;
        let x0 = cols.saturating_sub(w) / 2;
        let y0 = rows.saturating_sub(h) / 3;
        let bg = Style::DEFAULT.with_bg(BG_POP).with_fg(Color::Idx(252));
        let border = bg.with_fg(Color::Idx(244));
        for y in y0..(y0 + h).min(rows) {
            f.fill_row(y, x0, x0 + w, bg);
        }
        let title = format!(" {} ({}/{}) ", p.title, p.items.len(), p.all.len());
        for x in x0..x0 + w {
            f.set(y0, x, '\u{2500}', border);
        }
        f.put_str(y0, x0 + 2, &title, border.with(BOLD), x0 + w);
        let prompt = format!("> {}", p.query);
        let end = f.put_str(y0 + 1, x0 + 1, &prompt, bg.with(BOLD), x0 + w);
        let first = if p.sel >= shown { p.sel + 1 - shown } else { 0 };
        for k in 0..shown {
            let idx = first + k;
            let it = &p.items[idx];
            let sel = idx == p.sel;
            let st = if sel { Style::DEFAULT.with_bg(BG_POP_SEL).with_fg(Color::Idx(255)) } else { bg };
            f.fill_row(y0 + 2 + k, x0, x0 + w, st);
            f.put_str(y0 + 2 + k, x0 + 1, &it.label, st, x0 + w - 1);
        }
        if p.items.is_empty() {
            f.put_str(y0 + 2, x0 + 2, "no matches", bg.with_fg(C_DIM), x0 + w);
        }
        Some((y0 + 1, end))
    }
}

fn kind_letter(k: u8) -> char {
    match k {
        2 | 3 => 'f',  // method, function
        4 => 'c',      // constructor
        5 | 10 => 'v', // field, property
        6 => 'v',      // variable
        7 | 8 | 22 => 'T', // class, interface, struct
        9 => 'M',      // module
        13 | 20 => 'E',// enum, enum member
        14 => 'k',     // keyword
        15 => 's',     // snippet
        21 => 'C',     // constant
        _ => '\u{b7}',
    }
}

pub fn key_label(k: &crate::input::Key) -> String {
    use crate::input::Key::*;
    match k {
        Char(c) => c.to_string(),
        Ctrl(c) => format!("^{}", c.to_ascii_uppercase()),
        Esc => "<Esc>".into(),
        Enter => "<CR>".into(),
        Tab => "<Tab>".into(),
        _ => String::new(),
    }
}
