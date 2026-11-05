//! The embedded terminal pane: a shell on a PTY, rendered through the `Vt` emulator.

use std::io::Read;
use std::os::unix::io::{AsRawFd, RawFd};

use crate::editor::{Editor, Focus};
use crate::input::{Event, Key};
use crate::screen::{Color, Frame, Style, BOLD};
use crate::sys::Pty;
use crate::vt::Vt;

pub struct TermPane {
    pub pty: Pty,
    pub vt: Vt,
    /// lines scrolled back into history (0 = live view)
    pub scroll: usize,
    pub exited: bool,
}

impl TermPane {
    fn spawn(shell: &str, rows: usize, cols: usize) -> Result<TermPane, String> {
        let mut parts = shell.split_whitespace();
        let prog = parts.next().unwrap_or("/bin/sh").to_string();
        let args: Vec<String> = parts.map(|s| s.to_string()).collect();
        let cwd = std::env::current_dir().ok();
        let pty = Pty::spawn(&prog, &args, rows as u16, cols as u16, cwd.as_deref()).map_err(|e| format!("cannot start {}: {}", prog, e))?;
        Ok(TermPane { pty, vt: Vt::new(rows, cols), scroll: 0, exited: false })
    }

    pub fn fd(&self) -> RawFd {
        self.pty.master.as_raw_fd()
    }

    /// Read everything the shell has written. Returns false once the child is gone.
    pub fn pump(&mut self) -> bool {
        let mut buf = [0u8; 16384];
        let mut total = 0usize;
        loop {
            match (&self.pty.master).read(&mut buf) {
                Ok(0) => {
                    self.exited = true;
                    break;
                }
                Ok(n) => {
                    self.vt.feed(&buf[..n]);
                    total += n;
                    if total > 1 << 20 {
                        break; // let the UI breathe; poll will fire again
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    // EIO on Linux when the slave side is closed
                    self.exited = true;
                    break;
                }
            }
        }
        if !self.vt.responses.is_empty() {
            let r = std::mem::take(&mut self.vt.responses);
            self.pty.write_all(&r);
        }
        if self.scroll > self.vt.scrollback.len() {
            self.scroll = self.vt.scrollback.len();
        }
        !self.exited
    }

    /// True while the shell itself is reading the keyboard (a prompt), not a program it started.
    fn at_prompt(&self) -> bool {
        !self.vt.in_alt && self.pty.foreground_pgrp() == Some(self.pty.child.id())
    }

    fn key_bytes(&self, ev: &Event, prompt_keys: bool) -> Vec<u8> {
        encode_key(ev, self.vt.app_cursor, self.vt.bracketed_paste, prompt_keys && self.at_prompt())
    }
}

/// The bytes a key press becomes on the shell's input, the way xterm sends it, whatever the
/// host terminal used to report it (`ESC [ 1 ~`, `ESC O H`, `ESC [ 7 ~` … are all Home).
/// `app` is the shell's application cursor mode (DECCKM, `ESC [ ? 1 h`), which readline and zle
/// switch on while they edit a line: then the cursor keys are `ESC O x` instead of `ESC [ x`.
/// `prompt` says the shell is at its prompt: Home / End then become Ctrl-A / Ctrl-E, the one
/// spelling of "start / end of line" that bash, zsh and fish know without any key bindings
/// (zsh, for one, has none for the xterm sequences and silently drops them).
/// Text for pasting into the terminal program, without anything that could act as a command: an
/// ESC in it could carry its own "end of paste" (`ESC [ 201 ~`) and make the rest of the text run
/// as typed keys, and other control characters (Ctrl-C, Ctrl-D, Ctrl-Z, ...) would be taken for
/// keys even inside a bracketed paste by programs that do not honour it. Only tabs and line breaks
/// survive.
pub fn sanitize_paste(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' | '\t' => out.push(c),
            c if (c as u32) < 0x20 || c as u32 == 0x7f || (0x80..=0x9f).contains(&(c as u32)) => {}
            c => out.push(c),
        }
    }
    out
}

pub fn encode_key(ev: &Event, app: bool, bracketed_paste: bool, prompt: bool) -> Vec<u8> {
    let cursor = |c: u8| vec![0x1b, if app { b'O' } else { b'[' }, c];
    let tilde = |n: u8| format!("\x1b[{}~", n).into_bytes();
    // a modifier (Ctrl-Left, Shift-End, Ctrl-Delete …) comes as `ESC [ 1 ; 5 D`, which is already
    // what xterm sends, in either cursor mode: forward it untouched instead of dropping the modifier
    let modified = ev.raw.starts_with(b"\x1b[") && ev.raw.contains(&b';');
    match &ev.key {
        Key::Home if prompt => vec![0x01],
        Key::End if prompt => vec![0x05],
        Key::Up | Key::Down | Key::Right | Key::Left | Key::Home | Key::End | Key::Insert | Key::Delete | Key::PageUp | Key::PageDown | Key::F(_)
            if modified =>
        {
            ev.raw.clone()
        }
        Key::Up => cursor(b'A'),
        Key::Down => cursor(b'B'),
        Key::Right => cursor(b'C'),
        Key::Left => cursor(b'D'),
        Key::Home => cursor(b'H'),
        Key::End => cursor(b'F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F(n @ 1..=4) => vec![0x1b, b'O', b'P' + n - 1],
        Key::F(n) => match n {
            5 => tilde(15),
            6 => tilde(17),
            7 => tilde(18),
            8 => tilde(19),
            9 => tilde(20),
            10 => tilde(21),
            11 => tilde(23),
            12 => tilde(24),
            _ => ev.raw.clone(),
        },
        Key::Paste(s) => {
            let s = sanitize_paste(s);
            if bracketed_paste {
                let mut v = b"\x1b[200~".to_vec();
                v.extend_from_slice(s.as_bytes());
                v.extend_from_slice(b"\x1b[201~");
                v
            } else {
                s.replace('\n', "\r").into_bytes()
            }
        }
        _ => ev.raw.clone(),
    }
}

impl Editor {
    fn spawn_terminal(&mut self) -> Result<(), String> {
        let tr = self.term_rows_for_spawn();
        let t = TermPane::spawn(&self.opts.shell.clone(), tr, self.main_cols().max(10))?;
        self.term = Some(t);
        Ok(())
    }

    fn term_rows_for_spawn(&self) -> usize {
        let rows = self.main_rows();
        let want = rows * self.opts.term_height.clamp(10, 80) / 100;
        want.clamp(4, rows.saturating_sub(8).max(4))
    }

    /// Is the pane on screen? (A shell may be running without it: see `hide_terminal`.)
    pub fn term_visible(&self) -> bool {
        self.term.is_some() && !self.term_hidden
    }

    /// Alt-s: moves focus between the editor and the terminal pane. From the editor it shows the
    /// pane (starting a shell if there is none, unhiding it if it is hidden) and focuses it; from
    /// the pane, in either of its modes, it gives focus back to the editor. The shell keeps running.
    pub fn toggle_terminal(&mut self) {
        if self.focus == Focus::Editor {
            self.open_terminal();
        } else {
            self.focus = Focus::Editor;
        }
    }

    pub fn focus_terminal(&mut self) {
        if self.term.is_none() {
            if let Err(e) = self.spawn_terminal() {
                self.set_err(e);
                return;
            }
        }
        self.term_hidden = false;
        self.tree.focused = false;
        self.focus = Focus::TermInsert;
    }

    /// Alt-w: tuck the pane away without stopping its shell (output keeps being collected, and the
    /// shell's history is there when it comes back); pressed again it shows the pane. With no shell
    /// running there is nothing to hide.
    pub fn hide_terminal(&mut self) {
        if self.term.is_none() {
            self.set_msg("no terminal (Alt-q opens one)");
        } else if self.term_hidden {
            self.open_terminal();
        } else {
            self.term_hidden = true;
            self.focus = Focus::Editor;
            self.set_msg("terminal hidden: Alt-w shows it again, Alt-e stops the shell");
        }
    }

    /// Show the terminal pane (starting a shell if there is none) and focus it; also `:term`.
    pub fn open_terminal(&mut self) {
        // whatever insert mode had on screen (completion list, snippet placeholders) does not follow
        self.completion = None;
        self.snippet = None;
        self.focus_terminal();
    }

    /// Alt-e: close the terminal pane and stop its shell. Does nothing when there is none.
    pub fn close_terminal_key(&mut self) {
        if self.term.is_some() {
            self.close_terminal();
            self.set_msg("terminal closed");
        }
    }

    pub fn close_terminal(&mut self) {
        self.term = None; // dropping the Pty kills the shell
        self.term_hidden = false;
        self.focus = Focus::Editor;
    }

    pub fn term_fd(&self) -> Option<RawFd> {
        self.term.as_ref().map(|t| t.fd())
    }

    /// Called by the event loop when the PTY is readable.
    pub fn term_readable(&mut self) {
        let alive = match self.term.as_mut() {
            Some(t) => t.pump(),
            None => return,
        };
        if !alive {
            let status = self.term.as_mut().and_then(|t| t.pty.child.try_wait().ok().flatten());
            self.term = None;
            self.term_hidden = false;
            self.focus = Focus::Editor;
            match status {
                Some(s) if !s.success() => self.set_msg(format!("terminal exited ({})", s)),
                _ => self.set_msg("terminal closed"),
            }
        }
    }

    pub fn term_key(&mut self, ev: Event) {
        let prompt_keys = self.opts.term_prompt_keys;
        let Some(t) = self.term.as_mut() else {
            self.focus = Focus::Editor;
            return;
        };
        let half = (t.vt.rows / 2).max(1);
        let max = t.vt.scrollback.len();
        if self.focus == Focus::TermNormal {
            match ev.key {
                Key::Esc | Key::Char('i') | Key::Char('a') | Key::Enter => {
                    t.scroll = 0;
                    self.focus = Focus::TermInsert;
                }
                Key::Char('k') | Key::Up => t.scroll = (t.scroll + 1).min(max),
                Key::Char('j') | Key::Down => t.scroll = t.scroll.saturating_sub(1),
                Key::Ctrl('u') | Key::PageUp => t.scroll = (t.scroll + half).min(max),
                Key::Ctrl('d') | Key::PageDown => {
                    t.scroll = t.scroll.saturating_sub(half);
                    if t.scroll == 0 {
                        self.focus = Focus::TermInsert;
                    }
                }
                Key::Char('g') | Key::Home => t.scroll = max,
                Key::Char('G') | Key::End => t.scroll = 0,
                _ => {}
            }
            return;
        }
        match ev.key {
            Key::PageUp => {
                t.scroll = (t.scroll + half).min(max);
                if t.scroll > 0 && !t.vt.in_alt {
                    self.focus = Focus::TermNormal;
                } else if t.vt.in_alt {
                    t.scroll = 0;
                    t.pty.write_all(&ev.raw); // full-screen apps get PageUp themselves
                }
            }
            _ => {
                let bytes = t.key_bytes(&ev, prompt_keys);
                t.scroll = 0;
                t.pty.write_all(&bytes);
            }
        }
    }

    /// Resize the PTY and emulator to the pane geometry.
    fn sync_term_size(&mut self, rows: usize, cols: usize) {
        if let Some(t) = self.term.as_mut() {
            if t.vt.rows != rows || t.vt.cols != cols {
                t.vt.resize(rows, cols);
                t.pty.resize(rows as u16, cols as u16);
            }
        }
    }

    pub fn draw_terminal(&mut self, f: &mut Frame, header: usize, top: usize, rows: usize) {
        let cols = f.cols;
        self.sync_term_size(rows, cols);
        let focused = self.focus != Focus::Editor;
        let Some(t) = self.term.as_ref() else { return };
        let bar_bg = if focused { Color::Idx(24) } else { Color::Idx(237) };
        let bar = Style::DEFAULT.with_bg(bar_bg).with_fg(Color::Idx(252));
        f.fill_row(header, 0, cols, bar);
        let mut title = String::from(" TERMINAL");
        if !t.vt.title.is_empty() {
            title.push_str(" \u{2014} ");
            title.push_str(&t.vt.title.chars().take(40).collect::<String>());
        }
        if self.focus == Focus::TermNormal || t.scroll > 0 {
            title.push_str(&format!("  [scrollback {}/{}]", t.scroll, t.vt.scrollback.len()));
        }
        let x = f.put_str(header, 0, &title, bar.with(BOLD), cols);
        let hint = if self.focus == Focus::Editor {
            " Alt-s focus \u{b7} Alt-w hide \u{b7} Alt-e close "
        } else {
            " Alt-s editor \u{b7} Alt-w hide \u{b7} Alt-e close \u{b7} PgUp scroll "
        };
        let hw = hint.chars().count();
        if x + hw < cols {
            f.put_str(header, cols - hw, hint, bar.with_fg(Color::Idx(250)), cols);
        }
        for r in 0..rows {
            let row = t.vt.view_row(r, t.scroll);
            for (c, cell) in row.iter().enumerate().take(cols) {
                f.set(top + r, c, cell.ch, cell.style);
            }
        }
    }

    pub fn term_cursor(&self, top: usize, rows: usize, cols: usize) -> Option<(usize, usize)> {
        if self.focus != Focus::TermInsert {
            return None;
        }
        let t = self.term.as_ref()?;
        if t.scroll != 0 || !t.vt.cursor_visible || t.vt.cur.0 >= rows || t.vt.cur.1 >= cols {
            return None;
        }
        Some((top + t.vt.cur.0, t.vt.cur.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::InputParser;

    /// What the shell receives when the host terminal reports `host` for a key press.
    fn sent(host: &[u8], app: bool) -> String {
        let mut p = InputParser::new();
        p.push(host);
        let ev = p.next_event(true).expect("an event");
        assert!(p.next_event(true).is_none(), "{:?} is more than one key", host);
        String::from_utf8(encode_key(&ev, app, false, false)).unwrap().replace('\x1b', "<Esc>")
    }

    #[test]
    fn home_and_end_become_what_xterm_sends_however_the_host_reports_them() {
        for host in ["\x1b[H", "\x1b[1~", "\x1b[7~", "\x1bOH"] {
            assert_eq!(sent(host.as_bytes(), false), "<Esc>[H", "Home from {:?}", host);
            assert_eq!(sent(host.as_bytes(), true), "<Esc>OH", "Home from {:?} in application mode", host);
        }
        for host in ["\x1b[F", "\x1b[4~", "\x1b[8~", "\x1bOF"] {
            assert_eq!(sent(host.as_bytes(), false), "<Esc>[F", "End from {:?}", host);
            assert_eq!(sent(host.as_bytes(), true), "<Esc>OF", "End from {:?} in application mode", host);
        }
    }

    #[test]
    fn modified_cursor_keys_keep_their_modifier() {
        assert_eq!(sent(b"\x1b[1;5D", false), "<Esc>[1;5D"); // Ctrl-Left: back one word
        assert_eq!(sent(b"\x1b[1;5C", true), "<Esc>[1;5C");
        assert_eq!(sent(b"\x1b[1;2H", false), "<Esc>[1;2H"); // Shift-Home
        assert_eq!(sent(b"\x1b[3;5~", false), "<Esc>[3;5~"); // Ctrl-Delete
    }

    #[test]
    fn the_other_editing_keys_are_normalised_too() {
        for (host, want) in [("\x1b[A", "<Esc>[A"), ("\x1bOA", "<Esc>[A"), ("\x1b[2~", "<Esc>[2~"), ("\x1b[3~", "<Esc>[3~"), ("\x1b[5~", "<Esc>[5~"), ("\x1b[6~", "<Esc>[6~")] {
            assert_eq!(sent(host.as_bytes(), false), want, "from {:?}", host);
        }
        assert_eq!(sent(b"\x1b[B", true), "<Esc>OB");
        for (host, want) in [("\x1bOP", "<Esc>OP"), ("\x1b[11~", "<Esc>OP"), ("\x1bOS", "<Esc>OS"), ("\x1b[15~", "<Esc>[15~"), ("\x1b[24~", "<Esc>[24~")] {
            assert_eq!(sent(host.as_bytes(), false), want, "from {:?}", host);
        }
    }

    #[test]
    fn ordinary_keys_and_pastes_pass_through() {
        assert_eq!(sent(b"a", false), "a");
        assert_eq!(sent(b"\x01", false), "\u{1}"); // Ctrl-A
        assert_eq!(sent(b"\x1c", false), "\u{1c}"); // Ctrl-\ now belongs to the shell (SIGQUIT)
        assert_eq!(sent(b"\x1bb", false), "<Esc>b"); // Alt-b
        let ev = Event { key: Key::Paste("a\nb".into()), raw: vec![] };
        assert_eq!(encode_key(&ev, false, false, false), b"a\rb");
        assert_eq!(encode_key(&ev, false, true, false), b"\x1b[200~a\nb\x1b[201~");
    }

    #[test]
    fn pasted_text_cannot_end_the_paste_early_or_send_control_keys() {
        let evil = "echo hi\x1b[201~\rrm -rf ~\r\x03\x04\x1a\u{9b}31m";
        let ev = Event { key: Key::Paste(evil.into()), raw: vec![] };
        let bracketed = encode_key(&ev, false, true, false);
        let text = String::from_utf8(bracketed.clone()).unwrap();
        // exactly one start and one end marker: the ones we put there
        assert_eq!(text.matches("\x1b[200~").count(), 1);
        assert_eq!(text.matches("\x1b[201~").count(), 1);
        assert!(text.ends_with("\x1b[201~"));
        let inner = &text["\x1b[200~".len()..text.len() - "\x1b[201~".len()];
        assert!(!inner.chars().any(|c| c.is_control() && c != '\n' && c != '\t'), "{:?}", inner);
        let raw = encode_key(&ev, false, false, false);
        assert!(!raw.contains(&0x1b) && !raw.contains(&3) && !raw.contains(&4) && !raw.contains(&0x1a));
        assert_eq!(sanitize_paste("a\r\nb\rc\td"), "a\nb\nc\td");
    }

    #[test]
    fn at_a_shell_prompt_home_and_end_are_ctrl_a_and_ctrl_e() {
        let key = |host: &[u8], app: bool, prompt: bool| {
            let mut p = InputParser::new();
            p.push(host);
            encode_key(&p.next_event(true).unwrap(), app, false, prompt)
        };
        for host in ["\x1b[H", "\x1b[1~", "\x1bOH", "\x1b[7~", "\x1b[1;2H"] {
            for app in [false, true] {
                assert_eq!(key(host.as_bytes(), app, true), b"\x01", "Home from {:?}", host);
            }
        }
        for host in ["\x1b[F", "\x1b[4~", "\x1bOF", "\x1b[8~", "\x1b[1;5F"] {
            for app in [false, true] {
                assert_eq!(key(host.as_bytes(), app, true), b"\x05", "End from {:?}", host);
            }
        }
        // everything else is unchanged at a prompt, and Home / End are xterm's when it is a program
        assert_eq!(key(b"\x1b[D", false, true), b"\x1b[D");
        assert_eq!(key(b"\x1b[1;5D", false, true), b"\x1b[1;5D");
        assert_eq!(key(b"\x1b[3~", false, true), b"\x1b[3~");
        assert_eq!(key(b"\x1b[H", false, false), b"\x1b[H");
        assert_eq!(key(b"\x1b[F", true, false), b"\x1bOF");
    }
}
