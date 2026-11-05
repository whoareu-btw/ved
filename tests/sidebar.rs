#![allow(clippy::field_reassign_with_default)]
//! The project panel, the top bar, the mouse and the terminal keys, driven headlessly.
//!
//! The panel lists the working directory, which is process-wide: every test takes the lock and
//! works in a directory of its own.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ved::config::Options;
use ved::editor::{Editor, Focus, Mode};
use ved::input::{Event, Key, Mouse};
use ved::screen::Frame;

static CWD: Mutex<()> = Mutex::new(());

fn project(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ved-sidebar-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("src")).unwrap();
    std::fs::create_dir_all(d.join("target/debug")).unwrap();
    let main: String = (1..=60).map(|i| format!("line {}\n", i)).collect();
    std::fs::write(d.join("src/main.rs"), main).unwrap();
    std::fs::write(d.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    std::fs::write(d.join("README.md"), "# readme\n").unwrap();
    std::fs::write(d.join("Cargo.toml"), "[package]\n").unwrap();
    std::fs::write(d.join("target/debug/x.bin"), "x").unwrap();
    std::fs::canonicalize(&d).unwrap()
}

struct Env {
    ed: Editor,
    dir: PathBuf,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(std::env::temp_dir());
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn env_sized(name: &str, rows: usize, cols: usize) -> Env {
    let guard = CWD.lock().unwrap_or_else(|e| e.into_inner());
    let dir = project(name);
    std::env::set_current_dir(&dir).unwrap();
    let mut o = Options::default();
    o.lsp = false;
    o.osc52 = false;
    o.shell = "/bin/sh".into();
    let ed = Editor::new(o, rows, cols);
    Env { ed, dir, _guard: guard }
}

fn env(name: &str) -> Env {
    env_sized(name, 24, 100)
}

impl Env {
    fn open(&mut self, rel: &str) {
        let p = self.dir.join(rel);
        let i = self.ed.open_file(&p).unwrap();
        self.ed.switch_to(i);
    }

    fn screen(&mut self) -> Frame {
        let mut f = Frame::new(self.ed.rows, self.ed.cols);
        self.ed.draw(&mut f);
        f
    }

    fn key(&mut self, k: Key) {
        self.ed.handle_event(Event { key: k, raw: Vec::new() });
    }

    fn keys(&mut self, s: &str) {
        for c in s.chars() {
            self.key(match c {
                '\r' => Key::Enter,
                '\x1b' => Key::Esc,
                '\t' => Key::Tab,
                c => Key::Char(c),
            });
        }
    }

    fn click(&mut self, row: usize, col: usize) {
        self.key(Key::Mouse(Mouse { button: 0, row: row as u16, col: col as u16, release: false }));
        self.key(Key::Mouse(Mouse { button: 0, row: row as u16, col: col as u16, release: true }));
    }

    fn mouse(&mut self, button: u16, row: usize, col: usize, release: bool) {
        self.key(Key::Mouse(Mouse { button, row: row as u16, col: col as u16, release }));
    }

    /// Press the left button at (row, col), move to each of `path` (columns, same row) and release at the last.
    fn drag(&mut self, row: usize, from: usize, path: &[usize]) {
        self.mouse(0, row, from, false);
        for c in path {
            self.mouse(32, row, *c, false);
        }
        self.mouse(0, row, *path.last().unwrap_or(&from), true);
    }

    /// The columns of the tabs' x buttons on the top row.
    fn close_buttons(&mut self) -> Vec<usize> {
        let f = self.screen();
        (0..f.cols).filter(|&c| f.get(0, c).ch == '\u{d7}').collect()
    }

    fn wheel(&mut self, row: usize, col: usize, up: bool) {
        self.key(Key::Mouse(Mouse { button: if up { 64 } else { 65 }, row: row as u16, col: col as u16, release: false }));
    }

    /// The keyboard goes to the panel, with the row called `name` selected.
    fn panel_on(&mut self, name: &str) {
        let _ = self.screen();
        if !self.ed.tree.focused {
            self.key(Key::Ctrl('w'));
            self.key(Key::Char('h'));
        }
        let i = self.ed.tree.rows.iter().position(|r| r.name == name).unwrap_or_else(|| panic!("no row {}: {:?}", name, self.ed.tree.rows.iter().map(|r| r.name.clone()).collect::<Vec<_>>()));
        self.ed.tree.select(i);
    }

    /// What the command-line row shows (it is part of the editing area, right of the panel).
    fn cmd_row(&mut self) -> String {
        let f = self.screen();
        let sw = self.ed.sidebar_cols();
        f.row_text(f.rows - 1).chars().skip(sw).collect::<String>().trim_end().to_string()
    }

    fn selected(&self) -> String {
        self.ed.tree.rows.get(self.ed.tree.sel).map(|r| r.name.clone()).unwrap_or_default()
    }

    /// The row of the panel that shows `name` (screen row).
    fn panel_row(&mut self, name: &str) -> usize {
        let f = self.screen();
        let sw = self.ed.sidebar_cols();
        (0..f.rows).find(|&r| f.row_text(r).chars().take(sw).collect::<String>().contains(name)).unwrap_or_else(|| panic!("no `{}` in the panel:\n{}", name, f.text()))
    }
}

fn left(f: &Frame, r: usize, n: usize) -> String {
    f.row_text(r).chars().take(n).collect()
}

#[test]
fn the_panel_lists_the_project_and_follows_the_open_file() {
    let mut e = env("lists");
    let f = e.screen();
    let sw = e.ed.sidebar_cols();
    assert_eq!(sw, 30);
    // the bar: the button, the project name, and (right of the panel) the tab of the empty buffer
    let bar = f.row_text(0);
    assert!(bar.starts_with(" \u{2261} "), "{:?}", bar);
    let root = e.dir.file_name().unwrap().to_string_lossy().to_string();
    assert!(left(&f, 0, sw).contains(&root.chars().take(10).collect::<String>()), "{:?}", bar);
    // top-level entries: directories first, nothing from `.git`
    let names: Vec<String> = e.ed.tree.rows.iter().map(|r| r.name.clone()).collect();
    assert_eq!(names, ["src", "target", "Cargo.toml", "README.md"]);
    assert!(left(&f, 1, sw).contains("src"));
    // opening a file reveals it: its directory unfolds and its row is the selected one
    e.open("src/main.rs");
    let f = e.screen();
    assert_eq!(e.ed.tree.selected().unwrap().name, "main.rs");
    let r = e.panel_row("main.rs");
    assert!(left(&f, r, sw).contains("main.rs"));
    assert!(f.row_text(0).contains("main.rs"), "the tab: {:?}", f.row_text(0));
    // the text starts to the right of the panel, below the bar
    assert!(f.row_text(1).chars().skip(sw).collect::<String>().contains("line 1"), "{}", f.text());
    // the cursor is in the text, not in the panel
    let (cr, cc) = f.cursor.expect("a cursor");
    assert_eq!(cr, 1);
    assert!(cc >= sw, "cursor at column {}", cc);
}

#[test]
fn alt_b_and_the_button_hide_and_show_the_panel() {
    let mut e = env("hide");
    e.open("src/main.rs");
    assert_eq!(e.ed.sidebar_cols(), 30);
    // Alt-b hides it: the text moves to the left edge
    e.key(Key::Alt('b'));
    assert_eq!(e.ed.sidebar_cols(), 0);
    assert!(!e.ed.tree.focused);
    let f = e.screen();
    assert!(f.row_text(0).starts_with(" \u{2261} "), "the button stays: {:?}", f.row_text(0));
    assert!(f.row_text(1).starts_with("  1 line 1") || f.row_text(1).trim_start().starts_with("1 line 1"), "{:?}", f.row_text(1));
    // Alt-b shows it and gives it the keyboard
    e.key(Key::Alt('b'));
    assert_eq!(e.ed.sidebar_cols(), 30);
    assert!(e.ed.tree.focused);
    // the button: a click on it hides, another shows (without taking the keyboard)
    e.click(0, 1);
    assert_eq!(e.ed.sidebar_cols(), 0);
    e.click(0, 0);
    assert_eq!(e.ed.sidebar_cols(), 30);
    e.click(0, 2);
    assert_eq!(e.ed.sidebar_cols(), 0);
    // the `:` commands and options do the same
    e.keys(":tree\r");
    assert_eq!(e.ed.sidebar_cols(), 30);
    e.keys(":set nosidebar\r");
    assert_eq!(e.ed.sidebar_cols(), 0);
    e.keys(":set sidebar\r");
    assert_eq!(e.ed.sidebar_cols(), 30);
    e.keys(":set sidebarwidth=20\r");
    assert_eq!(e.ed.sidebar_cols(), 20);
    e.keys(":set sidebarwidth=2\r");
    assert_eq!(e.ed.sidebar_cols(), 16, "never narrower than 16");
    e.keys(":set sidebarwidth=5000\r");
    assert_eq!(e.ed.sidebar_cols(), 70, "as wide as the 100 columns leave room for (30 stay for the text)");
}

#[test]
fn the_panel_gives_way_on_a_narrow_or_short_terminal() {
    // the panel gets narrower before it gives up
    let e = env_sized("narrower", 24, 50);
    assert_eq!(e.ed.sidebar_cols(), 20, "50 columns: 30 for the text, 20 for the panel");
    drop(e);
    let e = env_sized("wide", 24, 200);
    let mut e = e;
    e.keys(":set sidebarwidth=5000\r");
    assert_eq!(e.ed.sidebar_cols(), 80, "never wider than 80");
    drop(e);
    let mut e = env_sized("narrow", 24, 40);
    assert_eq!(e.ed.sidebar_cols(), 0, "40 columns leave too little for a panel");
    let _ = e.screen();
    // Alt-b cannot focus a panel that is not there
    e.key(Key::Alt('b'));
    e.key(Key::Alt('b'));
    assert!(!e.ed.tree.focused || e.ed.sidebar_cols() > 0);
    e.keys("ix");
    assert_eq!(e.ed.buf().text(), "x\n", "keys still reach the text");
    drop(e); // the next environment takes the same lock
    let mut e = env_sized("short", 5, 100);
    assert_eq!(e.ed.bar_rows(), 0, "no bar on a 5-row screen");
    assert_eq!(e.ed.sidebar_cols(), 0);
    let f = e.screen();
    assert!(!f.row_text(0).contains('\u{2261}'));
}

#[test]
fn keyboard_navigation_in_the_panel() {
    let mut e = env("keys");
    // Ctrl-w h gives the panel the keyboard, and the status line says so
    e.key(Key::Ctrl('w'));
    e.key(Key::Char('h'));
    assert!(e.ed.tree.focused);
    let f = e.screen();
    assert!(f.row_text(f.rows - 2).contains("TREE"), "{}", f.row_text(f.rows - 2));
    assert!(f.cursor.is_none(), "no text cursor while the panel has the keyboard");
    assert_eq!(e.ed.tree.selected().unwrap().name, "src");
    // l opens a directory, l again steps into it, j/k move, h closes / goes to the parent
    e.keys("l");
    assert_eq!(e.ed.tree.rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["src", "lib.rs", "main.rs", "target", "Cargo.toml", "README.md"]);
    e.keys("l");
    assert_eq!(e.ed.tree.selected().unwrap().name, "lib.rs");
    e.keys("j");
    assert_eq!(e.ed.tree.selected().unwrap().name, "main.rs");
    e.keys("h");
    assert_eq!(e.ed.tree.selected().unwrap().name, "src");
    e.keys("h");
    assert_eq!(e.ed.tree.rows.len(), 4, "closed again");
    e.keys("G");
    assert_eq!(e.ed.tree.selected().unwrap().name, "README.md");
    e.keys("gg");
    assert_eq!(e.ed.tree.selected().unwrap().name, "src");
    e.keys("j");
    assert_eq!(e.ed.tree.selected().unwrap().name, "target");
    e.keys("jj");
    assert_eq!(e.ed.tree.selected().unwrap().name, "README.md");
    e.keys("k");
    assert_eq!(e.ed.tree.selected().unwrap().name, "Cargo.toml");
    e.keys("gg");
    e.keys("j");
    // typing in the panel never reaches the text
    let text = e.ed.buf().text();
    e.keys("x");
    e.keys("dd");
    e.keys("ihello");
    assert_eq!(e.ed.buf().text(), text);
    assert_eq!(e.ed.mode, Mode::Normal);
    // Enter on a file opens it and returns the keyboard to the editor
    e.keys("gg");
    e.keys("l");
    e.keys("l");
    e.keys("j");
    e.key(Key::Enter);
    assert!(!e.ed.tree.focused);
    assert!(e.ed.buf().path.as_ref().unwrap().ends_with("src/main.rs"));
    e.keys("jj");
    assert_eq!(e.ed.cursor().0, 2, "the keys go to the text again");
    // Esc and Ctrl-w l also give it back; `:` opens the command line from the panel
    e.key(Key::Ctrl('w'));
    e.key(Key::Char('h'));
    assert!(e.ed.tree.focused);
    e.key(Key::Esc);
    assert!(!e.ed.tree.focused);
    e.key(Key::Ctrl('w'));
    e.key(Key::Char('h'));
    e.key(Key::Ctrl('w'));
    e.key(Key::Char('l'));
    assert!(!e.ed.tree.focused);
    e.key(Key::Ctrl('w'));
    e.key(Key::Char('h'));
    e.keys(":");
    assert_eq!(e.ed.mode, Mode::Command);
    assert!(!e.ed.tree.focused);
    e.key(Key::Esc);
}

#[test]
fn opening_odd_rows_does_no_harm() {
    let mut e = env("odd");
    std::os::unix::fs::symlink("/", e.dir.join("rootlink")).unwrap();
    std::os::unix::fs::symlink(e.dir.join("nothing"), e.dir.join("dangling")).unwrap();
    let c = std::ffi::CString::new(e.dir.join("fifo").to_string_lossy().as_bytes()).unwrap();
    extern "C" {
        fn mkfifo(path: *const std::os::raw::c_char, mode: u32) -> i32;
    }
    assert_eq!(unsafe { mkfifo(c.as_ptr(), 0o600) }, 0);
    e.key(Key::Alt('b')); // hide
    e.key(Key::Alt('b')); // show and focus: lists again
    e.keys("R");
    let names: Vec<String> = e.ed.tree.rows.iter().map(|r| r.name.clone()).collect();
    for want in ["rootlink", "dangling", "fifo"] {
        assert!(names.iter().any(|n| n == want), "{:?}", names);
    }
    // Enter on each: a message, never a hang and never a buffer (a broken link must not become a
    // new empty file that a later :w would create)
    let before = e.ed.docs.len();
    let first = e.ed.docs[0].buf.path.clone();
    for want in ["rootlink", "dangling", "fifo"] {
        let i = e.ed.tree.rows.iter().position(|r| r.name == want).unwrap();
        e.ed.tree.select(i);
        e.key(Key::Enter);
        assert_eq!(e.ed.docs.len(), before, "{} must not open", want);
        assert!(e.ed.tree.focused, "still in the panel after {}", want);
        assert_eq!(e.ed.docs[0].buf.path, first, "{} must not replace the empty buffer", want);
    }
    assert!(!e.dir.join("nothing").exists());
}

#[test]
fn clicking_the_panel_the_tabs_and_the_text() {
    let mut e = env("clicks");
    e.open("src/main.rs");
    e.open("README.md");
    e.open("src/lib.rs");
    let sw = e.ed.sidebar_cols();
    // tabs: one per buffer, a click switches to it
    let f = e.screen();
    let bar = f.row_text(0);
    for n in ["main.rs", "README.md", "lib.rs"] {
        assert!(bar.contains(n), "{:?}", bar);
    }
    let col_of = |name: &str| bar.chars().collect::<Vec<_>>().windows(name.len()).position(|w| w.iter().collect::<String>() == name).unwrap();
    e.click(0, col_of("main.rs") + 1);
    assert!(e.ed.buf().path.as_ref().unwrap().ends_with("src/main.rs"));
    e.click(0, col_of("README.md") + 2);
    assert!(e.ed.buf().path.as_ref().unwrap().ends_with("README.md"));
    // a click on a directory row opens / closes it and keeps the panel's keyboard
    let r = e.panel_row("src");
    e.click(r, 3);
    assert!(e.ed.tree.focused);
    assert!(!e.ed.tree.rows.iter().any(|r| r.name == "lib.rs") || e.ed.tree.rows.iter().any(|r| r.name == "lib.rs"));
    let was_open = e.ed.tree.rows.iter().find(|r| r.name == "src").unwrap().open;
    e.click(r, 3);
    let now_open = e.ed.tree.rows.iter().find(|r| r.name == "src").unwrap().open;
    assert_ne!(was_open, now_open);
    // a click on a file row opens it and hands the keyboard to the editor
    if !e.ed.tree.rows.iter().find(|r| r.name == "src").unwrap().open {
        let r = e.panel_row("src");
        e.click(r, 3);
    }
    let r = e.panel_row("main.rs");
    e.click(r, 6);
    assert!(!e.ed.tree.focused);
    assert!(e.ed.buf().path.as_ref().unwrap().ends_with("src/main.rs"));
    // a click in the text moves the cursor there: row 3 of the text is line 3, column after the gutter
    let f = e.screen();
    let gw = e.ed.gutter_width();
    e.click(1 + 2, sw + gw + 3);
    assert_eq!(e.ed.cursor(), (2, 3));
    // below the last line: the last line; past the end of a line: its last character
    e.click(20, sw + gw + 40);
    assert_eq!(e.ed.cursor().0, 19.min(e.ed.buf().line_count() - 1));
    let _ = f;
    // a click in the gutter puts the cursor at the start of that line
    e.click(1 + 4, sw + 1);
    assert_eq!(e.ed.cursor(), (4, 0));
    // a click on the bar's empty part, the status line or the panel's edge changes nothing
    let at = e.ed.cursor();
    e.click(0, 90);
    e.click(22, 50);
    e.click(5, sw - 1);
    assert_eq!(e.ed.cursor(), at);
}

#[test]
fn clicking_in_insert_mode_moves_the_cursor_and_keeps_typing() {
    let mut e = env("insert");
    e.open("src/main.rs");
    let sw = e.ed.sidebar_cols();
    let gw = e.ed.gutter_width();
    e.keys("i");
    e.click(1 + 1, sw + gw + 2);
    assert_eq!(e.ed.mode, Mode::Insert);
    assert_eq!(e.ed.cursor(), (1, 2));
    e.keys("X");
    assert_eq!(e.ed.buf().lines[1].iter().collect::<String>(), "liXne 2");
    e.key(Key::Esc);
    e.keys("u");
    assert_eq!(e.ed.buf().lines[1].iter().collect::<String>(), "line 2");
}

#[test]
fn the_wheel_scrolls_the_text_the_panel_and_a_popup() {
    let mut e = env("wheel");
    e.open("src/main.rs");
    let sw = e.ed.sidebar_cols();
    let _ = e.screen();
    let top0 = e.ed.docs[e.ed.cur].top;
    e.wheel(5, sw + 10, false);
    e.wheel(5, sw + 10, false);
    let _ = e.screen();
    let d = &e.ed.docs[e.ed.cur];
    assert_eq!(d.top, top0 + 6);
    assert!(d.cursor.0 >= d.top, "the cursor stays inside the window");
    e.wheel(5, sw + 10, true);
    let _ = e.screen();
    assert_eq!(e.ed.docs[e.ed.cur].top, top0 + 3);
    for _ in 0..100 {
        e.wheel(5, sw + 10, true);
    }
    let _ = e.screen();
    assert_eq!(e.ed.docs[e.ed.cur].top, 0);
    assert!(e.ed.cursor().0 < e.ed.view_h, "the cursor was carried along with the window");
    for _ in 0..100 {
        e.wheel(5, sw + 10, false);
    }
    let f = e.screen();
    let d = &e.ed.docs[e.ed.cur];
    assert!(d.top < d.buf.line_count());
    assert!(d.cursor.0 < d.buf.line_count());
    assert!(f.cursor.is_some());
    // the panel scrolls without moving its selection
    for i in 0..60 {
        std::fs::write(e.dir.join(format!("f{:03}.txt", i)), "").unwrap();
    }
    e.keys(":cd .\r");
    let _ = e.screen();
    let sel = e.ed.tree.sel;
    e.wheel(5, 5, false);
    let _ = e.screen();
    assert_eq!(e.ed.tree.top, 3);
    assert_eq!(e.ed.tree.sel, sel);
    for _ in 0..100 {
        e.wheel(5, 5, false);
    }
    let _ = e.screen();
    assert!(e.ed.tree.top <= e.ed.tree.rows.len());
    // a popup scrolls too
    e.keys(":help\r");
    assert!(e.ed.popup.is_some());
    e.wheel(5, 50, false);
    assert_eq!(e.ed.popup.as_ref().unwrap().scroll, 3);
    e.wheel(5, 50, true);
    assert_eq!(e.ed.popup.as_ref().unwrap().scroll, 0);
}

#[test]
fn the_mouse_can_be_switched_off_and_never_enters_macros_or_the_command_line() {
    let mut e = env("nomouse");
    e.open("src/main.rs");
    let sw = e.ed.sidebar_cols();
    let gw = e.ed.gutter_width();
    // recording a macro: a click is not recorded as a key
    e.keys("qa");
    e.click(1 + 3, sw + gw);
    e.keys("q");
    assert_eq!(e.ed.regs.get(&'a').map(|r| r.text.iter().collect::<String>()), None::<String>);
    // in the command line, clicks do nothing
    e.keys(":set");
    e.click(5, sw + gw + 2);
    assert_eq!(e.ed.mode, Mode::Command);
    assert_eq!(e.ed.cmdline, "set");
    e.key(Key::Esc);
    e.keys(":set nomouse\r");
    let at = e.ed.cursor();
    e.click(1 + 6, sw + gw + 1);
    assert_eq!(e.ed.cursor(), at, "with nomouse a click does nothing");
    e.click(0, 1);
    assert_eq!(e.ed.sidebar_cols(), 30);
}

#[test]
fn terminal_keys_open_hide_show_and_close() {
    let mut e = env("termkeys");
    e.open("src/main.rs");
    assert!(e.ed.term.is_none());
    // Alt-w / Alt-e with no shell do nothing harmful
    e.key(Key::Alt('w'));
    e.key(Key::Alt('e'));
    assert!(e.ed.term.is_none());
    // Alt-q opens the terminal and focuses it
    e.key(Key::Alt('q'));
    assert!(e.ed.term.is_some());
    assert_eq!(e.ed.focus, Focus::TermInsert);
    let f = e.screen();
    assert!(f.text().contains("TERMINAL"), "{}", f.text());
    assert!(f.text().contains("Alt-w hide") && f.text().contains("Alt-e close"), "{}", f.text());
    // the pane sits to the right of the panel, under the text
    let sw = e.ed.sidebar_cols();
    let header = (0..f.rows).find(|&r| f.row_text(r).contains("TERMINAL")).unwrap();
    assert!(f.row_text(header).chars().skip(sw).collect::<String>().contains("TERMINAL"));
    // Alt-w hides it: the shell lives on, nothing is drawn, the editor has the keyboard
    e.key(Key::Alt('w'));
    assert!(e.ed.term.is_some());
    assert!(e.ed.term_hidden);
    assert_eq!(e.ed.focus, Focus::Editor);
    let f = e.screen();
    assert!(!f.text().contains("TERMINAL"), "{}", f.text());
    e.keys("jj");
    assert_eq!(e.ed.cursor().0, 2);
    // Alt-w again shows it and focuses it; Alt-s then hands the keyboard back, and once more takes it
    e.key(Key::Alt('w'));
    assert!(!e.ed.term_hidden);
    assert_eq!(e.ed.focus, Focus::TermInsert);
    assert!(e.screen().text().contains("TERMINAL"));
    e.key(Key::Alt('s'));
    assert_eq!(e.ed.focus, Focus::Editor);
    e.key(Key::Alt('s'));
    assert_eq!(e.ed.focus, Focus::TermInsert);
    // Alt-q on a hidden pane shows it; Alt-s on a hidden pane too
    e.key(Key::Alt('w'));
    e.key(Key::Alt('q'));
    assert!(!e.ed.term_hidden && e.ed.focus == Focus::TermInsert);
    e.key(Key::Alt('w'));
    e.key(Key::Alt('s'));
    assert!(!e.ed.term_hidden && e.ed.focus == Focus::TermInsert);
    // Alt-b belongs to the shell while the terminal has the keyboard: the panel stays
    let cols = e.ed.sidebar_cols();
    e.key(Key::Alt('b'));
    assert_eq!(e.ed.sidebar_cols(), cols);
    // Alt-e closes it and stops the shell
    e.key(Key::Alt('e'));
    assert!(e.ed.term.is_none());
    assert!(!e.ed.term_hidden);
    assert_eq!(e.ed.focus, Focus::Editor);
    assert!(!e.screen().text().contains("TERMINAL"));
    // hidden, then closed: the next open starts a fresh shell, visible
    e.key(Key::Alt('q'));
    e.key(Key::Alt('w'));
    e.key(Key::Alt('e'));
    assert!(e.ed.term.is_none() && !e.ed.term_hidden);
    e.key(Key::Alt('q'));
    assert!(e.ed.term.is_some() && !e.ed.term_hidden);
    e.key(Key::Alt('e'));
    // while a command line is being typed the terminal keys are just typing
    e.keys(":");
    e.key(Key::Alt('q'));
    assert!(e.ed.term.is_none());
    e.key(Key::Esc);
}

#[test]
fn clicking_the_terminal_pane_focuses_it_and_the_text_takes_it_back() {
    let mut e = env("termclick");
    e.open("src/main.rs");
    e.key(Key::Alt('q'));
    e.key(Key::Alt('s')); // keyboard back to the editor, pane still shown
    assert_eq!(e.ed.focus, Focus::Editor);
    let f = e.screen();
    let sw = e.ed.sidebar_cols();
    let header = (0..f.rows).find(|&r| f.row_text(r).contains("TERMINAL")).unwrap();
    e.click(header + 2, sw + 10);
    assert_eq!(e.ed.focus, Focus::TermInsert);
    // a click in the text takes the keyboard back
    e.click(2, sw + 8);
    assert_eq!(e.ed.focus, Focus::Editor);
    // the wheel over the pane scrolls its history without moving the keyboard
    e.wheel(header + 2, sw + 10, true);
    assert_eq!(e.ed.focus, Focus::Editor);
    e.key(Key::Alt('e'));
}

#[test]
fn the_panel_follows_cd_and_saves() {
    let mut e = env("follow");
    let _ = e.screen();
    assert!(e.ed.tree.rows.iter().any(|r| r.name == "src"));
    // a new file appears after a write
    e.keys(":e brand_new.txt\r");
    e.keys("ihello");
    e.key(Key::Esc);
    e.keys(":w\r");
    assert!(e.ed.tree.rows.iter().any(|r| r.name == "brand_new.txt"), "{:?}", e.ed.tree.rows.iter().map(|r| &r.name).collect::<Vec<_>>());
    // :cd re-roots the panel
    e.keys(":cd src\r");
    let _ = e.screen();
    let names: Vec<String> = e.ed.tree.rows.iter().map(|r| r.name.clone()).collect();
    assert_eq!(names, ["lib.rs", "main.rs"]);
    assert_eq!(e.ed.tree.root_name(), "src");
    let _ = Path::new("");
}

#[test]
fn the_file_picker_does_not_chase_symlink_loops() {
    let mut e = env("loops");
    // eight links back to the project itself: followed, they would be 8^10 directories to read
    for i in 0..8 {
        std::os::unix::fs::symlink(".", e.dir.join(format!("loop{}", i))).unwrap();
    }
    std::os::unix::fs::symlink("README.md", e.dir.join("readme-link")).unwrap();
    std::os::unix::fs::symlink("nowhere", e.dir.join("dangling")).unwrap();
    let started = std::time::Instant::now();
    e.keys(":files\r");
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "the walk took {:?}", started.elapsed());
    let p = e.ed.picker.as_ref().expect("the picker is open");
    let names: Vec<&str> = p.all.iter().map(|i| i.label.as_str()).collect();
    assert!(names.contains(&"README.md") && names.contains(&"src/main.rs"), "{:?}", names);
    assert!(names.contains(&"readme-link"), "a link to a file is a file: {:?}", names);
    assert!(!names.iter().any(|n| n.starts_with("loop") || *n == "dangling"), "{:?}", names);
}

#[test]
fn the_panels_edge_is_dragged_with_the_mouse() {
    let mut e = env("drag");
    e.open("src/main.rs");
    let _ = e.screen();
    assert_eq!(e.ed.sidebar_cols(), 30);
    let cursor = e.ed.cursor();
    let sel = e.ed.tree.sel;
    // press on the edge (the rule is the last column of the panel), move, release
    e.mouse(0, 5, 29, false);
    assert!(e.ed.sidebar_drag);
    e.mouse(32, 5, 38, false);
    assert_eq!(e.ed.sidebar_cols(), 39, "the edge follows the pointer");
    let f = e.screen();
    assert_eq!(f.get(5, 38).ch, '\u{2502}');
    assert_eq!(f.get(5, 38).style.fg, ved::screen::Color::Idx(75), "the edge is highlighted while it is dragged");
    assert_eq!(f.get(0, 38).style.fg, ved::screen::Color::Idx(75));
    e.mouse(32, 5, 44, false);
    e.mouse(0, 5, 44, true);
    assert!(!e.ed.sidebar_drag);
    assert_eq!(e.ed.sidebar_cols(), 45);
    assert!(e.ed.msg.contains("panel width 45"), "{:?}", e.ed.msg);
    let f = e.screen();
    assert_eq!(f.get(5, 44).style.fg, ved::screen::Color::Idx(238), "back to the normal colour");
    // the text area and the tabs moved with it
    assert!(f.row_text(0).chars().skip(45).collect::<String>().contains("main.rs"), "{:?}", f.row_text(0));
    // nothing else happened: no click in the tree or the text, no focus change
    assert_eq!(e.ed.cursor(), cursor);
    assert_eq!(e.ed.tree.sel, sel);
    assert!(!e.ed.tree.focused);
    assert_eq!(e.ed.docs.len(), 1);
    // too narrow, too wide: kept inside the limits (and the 30 columns the text needs)
    e.drag(5, 44, &[30, 10, 3]);
    assert_eq!(e.ed.sidebar_cols(), 16);
    e.drag(5, 15, &[40, 90, 500, 65535]);
    assert_eq!(e.ed.sidebar_cols(), 70);
    // the rule at the top row is an edge too
    e.drag(0, 69, &[50]);
    assert_eq!(e.ed.sidebar_cols(), 51);
    // a click on the edge without moving changes nothing
    e.drag(5, 50, &[]);
    assert_eq!(e.ed.sidebar_cols(), 51);
}

#[test]
fn a_drag_cannot_get_stuck_and_never_reaches_the_text() {
    let mut e = env("dragstuck");
    e.open("src/main.rs");
    let _ = e.screen();
    // a key press ends a drag whose release was lost
    e.mouse(0, 5, 29, false);
    assert!(e.ed.sidebar_drag);
    e.key(Key::Char('j'));
    assert!(!e.ed.sidebar_drag);
    e.mouse(32, 5, 50, false);
    assert_eq!(e.ed.sidebar_cols(), 30, "motion without a drag does nothing");
    // so does any other mouse event: a release of another button, a new press
    e.mouse(0, 5, 29, false);
    e.mouse(35, 5, 50, false);
    assert!(!e.ed.sidebar_drag);
    assert_eq!(e.ed.sidebar_cols(), 30);
    // the wheel does not end it
    e.mouse(0, 5, 29, false);
    e.wheel(5, 50, false);
    assert!(e.ed.sidebar_drag);
    e.mouse(0, 5, 40, true);
    assert_eq!(e.ed.sidebar_cols(), 41);
    // turning the mouse off in the middle of a drag ends it
    e.mouse(0, 5, 40, false);
    assert!(e.ed.sidebar_drag);
    e.keys(":set nomouse\r");
    assert!(!e.ed.sidebar_drag);
    e.keys(":set mouse\r");
    // motion with the button held in the text selects nothing and moves nothing
    e.mouse(0, 4, 60, false);
    let at = e.ed.cursor();
    e.mouse(32, 8, 70, false);
    e.mouse(32, 9, 20, false);
    e.mouse(0, 9, 20, true);
    assert_eq!(e.ed.cursor(), at);
    assert_eq!(e.ed.mode, Mode::Normal);
    // a drag that starts anywhere but on the edge is not a drag
    e.mouse(0, 3, 10, false);
    assert!(!e.ed.sidebar_drag);
    // nothing to drag when the panel is hidden
    e.keys(":set nosidebar\r");
    e.mouse(0, 5, 29, false);
    assert!(!e.ed.sidebar_drag);
}

#[test]
fn tabs_have_an_x_that_closes_them() {
    let mut e = env("tabsx");
    e.open("src/main.rs");
    e.open("src/lib.rs");
    e.open("README.md");
    assert_eq!(e.ed.docs.len(), 3);
    let xs = e.close_buttons();
    assert_eq!(xs.len(), 3, "an x on every tab: {:?}\n{}", xs, e.screen().row_text(0));
    // x of the middle tab (not the current one): it goes, the current buffer stays
    let here = e.ed.docs[e.ed.cur].buf.path.clone();
    e.click(0, xs[1]);
    assert_eq!(e.ed.docs.len(), 2);
    assert_eq!(e.ed.docs[e.ed.cur].buf.path, here, "the current buffer is still the current buffer");
    assert!(e.ed.docs.iter().all(|d| d.buf.path.as_ref().is_none_or(|p| !p.ends_with("lib.rs"))));
    assert!(e.ed.alt.is_none_or(|a| a < e.ed.docs.len()));
    // the space on each side of the x is part of the button
    let xs = e.close_buttons();
    assert_eq!(xs.len(), 2);
    e.click(0, xs[0] + 1);
    assert_eq!(e.ed.docs.len(), 1);
    assert!(e.ed.docs[0].buf.path.as_ref().unwrap().ends_with("README.md"));
    // the last buffer: its x leaves an empty one, the editor does not quit
    let xs = e.close_buttons();
    e.click(0, xs[0] - 1);
    assert_eq!(e.ed.docs.len(), 1);
    assert!(e.ed.docs[0].buf.path.is_none());
    assert!(!e.ed.quit);
    assert_eq!(e.close_buttons().len(), 1, "even the empty buffer has its tab");
}

#[test]
fn clicking_the_name_of_a_tab_still_switches_and_the_middle_button_closes() {
    let mut e = env("tabname");
    e.open("src/main.rs");
    e.open("src/lib.rs");
    let f = e.screen();
    let sw = e.ed.sidebar_cols();
    let tabs = e.ed.tab_layout(sw, f.cols);
    assert_eq!(tabs.len(), 2);
    let (first, a, _) = tabs[0];
    e.click(0, a + 2);
    assert_eq!(e.ed.cur, first, "a click on the name switches to the buffer");
    assert_eq!(e.ed.docs.len(), 2);
    // middle button on the other tab closes it
    let (_, a2, _) = tabs[1];
    e.mouse(1, 0, a2 + 2, false);
    e.mouse(1, 0, a2 + 2, true);
    assert_eq!(e.ed.docs.len(), 1);
    assert!(e.ed.docs[0].buf.path.as_ref().unwrap().ends_with("main.rs"));
}

#[test]
fn a_tab_with_unsaved_changes_needs_a_second_click() {
    let mut e = env("tabdirty");
    e.open("src/main.rs");
    e.open("src/lib.rs");
    e.keys("ixyz\x1b");
    assert!(e.ed.buf().modified);
    let xs = e.close_buttons();
    // the current, modified tab: the first click only asks
    e.click(0, xs[1]);
    assert_eq!(e.ed.docs.len(), 2, "not closed by one click");
    assert!(e.ed.msg.contains("unsaved"), "{:?}", e.ed.msg);
    assert!(e.ed.buf().modified);
    // a click somewhere else withdraws the question
    e.click(10, 60);
    let xs = e.close_buttons();
    e.click(0, xs[1]);
    assert_eq!(e.ed.docs.len(), 2, "asks again");
    // so does a key
    e.keys("l");
    let xs = e.close_buttons();
    e.click(0, xs[1]);
    assert_eq!(e.ed.docs.len(), 2);
    // the second click in a row discards it
    e.click(0, xs[1]);
    assert_eq!(e.ed.docs.len(), 1);
    assert!(e.ed.docs[0].buf.path.as_ref().unwrap().ends_with("main.rs"));
    // the file on disk was never touched
    assert_eq!(std::fs::read_to_string(e.dir.join("src/lib.rs")).unwrap(), "pub fn f() {}\n");
    // a modified tab that is not the current one: one click asks, two close it, the current buffer stays
    e.open("src/lib.rs");
    e.keys("ixyz\x1b");
    e.open("README.md");
    let before = e.ed.docs[e.ed.cur].buf.path.clone();
    let xs = e.close_buttons();
    assert_eq!(xs.len(), 3);
    e.click(0, xs[1]);
    assert_eq!(e.ed.docs.len(), 3);
    e.click(0, xs[1]);
    assert_eq!(e.ed.docs.len(), 2);
    assert_eq!(e.ed.docs[e.ed.cur].buf.path, before);
}

#[test]
fn the_cursor_goes_all_the_way_to_the_bottom_row_before_the_window_scrolls() {
    let mut e = env("scrollrows");
    e.open("src/main.rs");
    let _ = e.screen();
    let h = e.ed.view_h;
    assert!((10..60).contains(&h));
    // `j` down to the last visible row: the window has not moved yet
    e.keys(&"j".repeat(h - 1));
    let _ = e.screen();
    assert_eq!(e.ed.cursor().0, h - 1);
    assert_eq!(e.ed.docs[e.ed.cur].top, 0, "the cursor reaches the last row first");
    let f = e.screen();
    let last_text_row = 1 + h - 1;
    assert!(f.row_text(last_text_row).contains(&format!("line {}", h)), "{:?}", f.row_text(last_text_row));
    // one more and the window scrolls by exactly one line
    e.keys("j");
    let _ = e.screen();
    assert_eq!(e.ed.docs[e.ed.cur].top, 1);
    // and back up: the cursor reaches the first row
    e.keys(&"k".repeat(h - 1));
    let _ = e.screen();
    assert_eq!(e.ed.docs[e.ed.cur].top, 1, "the cursor is on the first row, the window has not moved");
    e.keys("k");
    let _ = e.screen();
    assert_eq!(e.ed.docs[e.ed.cur].top, 0);
    // the margin is still there for whoever wants it
    e.keys(":set scrolloff=4\r");
    e.keys("gg");
    e.keys(&"j".repeat(h - 5));
    let _ = e.screen();
    assert_eq!(e.ed.docs[e.ed.cur].top, 0);
    e.keys("j");
    let _ = e.screen();
    assert_eq!(e.ed.docs[e.ed.cur].top, 1, "with scrolloff=4 the window moves 4 rows early");
}

// ---- new file, new folder, delete ------------------------------------------------------------

#[test]
fn a_makes_a_file_next_to_the_selected_one_and_opens_it() {
    let mut e = env("newfile");
    e.panel_on("README.md");
    e.keys("a");
    assert!(e.ed.prompt.is_some());
    assert_eq!(e.cmd_row(), "New file:");
    e.keys("notes.md");
    assert_eq!(e.cmd_row(), "New file: notes.md");
    e.keys("\r");
    let p = e.dir.join("notes.md");
    assert!(p.is_file() && std::fs::read(&p).unwrap().is_empty());
    assert!(e.ed.prompt.is_none());
    assert_eq!(e.ed.docs[e.ed.cur].buf.path.as_deref(), Some(p.as_path()), "the new file is open");
    assert!(!e.ed.tree.focused, "the keyboard went to the editor");
    assert!(e.ed.msg.contains("created notes.md"), "{:?}", e.ed.msg);
    assert_eq!(e.selected(), "notes.md");
    // it can be typed into and saved
    e.keys("ihello\x1b:w\r");
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello\n");
}

#[test]
fn a_in_a_folder_makes_the_file_inside_it_and_nested_names_make_the_folders() {
    let mut e = env("newnested");
    e.panel_on("src");
    e.keys("a");
    assert_eq!(e.cmd_row(), "New file in src/:");
    e.keys("new.rs\r");
    assert!(e.dir.join("src/new.rs").is_file());
    assert!(e.ed.tree.rows.iter().any(|r| r.name == "new.rs"), "the folder was opened to show it");
    // nested, from a file inside a folder: the folder of that file
    e.panel_on("lib.rs");
    e.keys("a");
    assert_eq!(e.cmd_row(), "New file in src/:");
    e.keys("deep/er/x.txt\r");
    assert!(e.dir.join("src/deep/er/x.txt").is_file());
    // a name that ends in / is a folder
    e.panel_on("README.md");
    e.keys("afolder/\r");
    assert!(e.dir.join("folder").is_dir());
    assert_ne!(e.ed.docs[e.ed.cur].buf.path.as_deref(), Some(e.dir.join("folder").as_path()));
}

#[test]
fn f_makes_a_folder_and_keeps_the_panel() {
    let mut e = env("newfolder");
    e.panel_on("README.md");
    e.keys("f");
    assert_eq!(e.cmd_row(), "New folder:");
    e.keys("assets\r");
    assert!(e.dir.join("assets").is_dir());
    assert!(e.ed.tree.focused, "still in the panel");
    assert_eq!(e.selected(), "assets");
    assert!(e.ed.msg.contains("created folder assets/"), "{:?}", e.ed.msg);
    assert!(e.ed.docs.iter().all(|d| d.buf.path.is_none()), "no buffer was opened");
    // nested folders, and inside the selected one
    e.keys("fa/b/c\r");
    assert!(e.dir.join("assets/a/b/c").is_dir());
    assert!(e.ed.tree.rows.iter().any(|r| r.name == "c"), "revealed");
}

#[test]
fn typing_a_name_is_only_typing() {
    let mut e = env("typing");
    e.panel_on("README.md");
    // keys that mean something in the panel, in the editor and for the terminal are letters here
    e.keys("a");
    e.keys("dj:wq.txt");
    e.key(Key::Alt('q'));
    e.key(Key::Alt('s'));
    e.key(Key::Alt('b'));
    e.key(Key::Up);
    e.key(Key::Tab);
    assert!(e.ed.prompt.is_some());
    assert_eq!(e.cmd_row(), "New file: dj:wq.txt");
    assert!(e.ed.term.is_none(), "Alt-q did not start a terminal");
    assert!(e.ed.sidebar_cols() > 0, "Alt-b did not hide the panel");
    // editing keys
    e.key(Key::Backspace);
    e.keys("x");
    assert_eq!(e.cmd_row(), "New file: dj:wq.txx");
    e.key(Key::Ctrl('u'));
    assert_eq!(e.cmd_row(), "New file:");
    e.keys("src/some thing");
    e.key(Key::Ctrl('w'));
    assert_eq!(e.cmd_row(), "New file: src/some");
    e.key(Key::Ctrl('w'));
    assert_eq!(e.cmd_row(), "New file: src/");
    e.key(Key::Paste("pasted.txt\nsecond line".into()));
    assert_eq!(e.cmd_row(), "New file: src/pasted.txt");
    // the mouse does nothing while a question is up
    let r = e.panel_row("Cargo.toml");
    e.click(r, 5);
    assert!(e.ed.prompt.is_some());
    assert_eq!(e.selected(), "README.md");
    // Esc gives up; so does Backspace on nothing
    e.key(Key::Esc);
    assert!(e.ed.prompt.is_none());
    assert!(!e.dir.join("src/pasted.txt").exists());
    e.keys("a");
    e.key(Key::Backspace);
    assert!(e.ed.prompt.is_none());
    // and the panel works again
    e.keys("k");
    assert_eq!(e.selected(), "Cargo.toml");
}

#[test]
fn bad_names_make_nothing() {
    let mut e = env("badnames");
    e.panel_on("README.md");
    let outside = e.dir.parent().unwrap().to_path_buf();
    let before: Vec<String> = std::fs::read_dir(&e.dir).unwrap().flatten().map(|d| d.file_name().to_string_lossy().to_string()).collect();
    for bad in ["../escape.txt", "/tmp/ved-abs-escape.txt", "a/../../escape", ".git", ".git/config", "x\u{1b}y", "   ", "src/lib.rs/inside", "README.md", "src"] {
        e.panel_on("README.md");
        e.keys("a");
        e.keys(bad);
        e.keys("\r");
        assert!(e.ed.prompt.is_none());
        assert!(e.ed.msg_err, "{:?} should have been refused, got {:?}", bad, e.ed.msg);
    }
    assert!(!outside.join("escape.txt").exists() && !outside.join("escape").exists());
    assert!(!Path::new("/tmp/ved-abs-escape.txt").exists());
    let after: Vec<String> = std::fs::read_dir(&e.dir).unwrap().flatten().map(|d| d.file_name().to_string_lossy().to_string()).collect();
    let (mut b, mut a) = (before, after);
    b.sort();
    a.sort();
    assert_eq!(a, b, "nothing was created in the project");
    assert_eq!(std::fs::read_to_string(e.dir.join("README.md")).unwrap(), "# readme\n", "an existing file is not overwritten");
    assert_eq!(std::fs::read_to_string(e.dir.join("src/lib.rs")).unwrap(), "pub fn f() {}\n");
}

#[test]
fn new_names_do_not_go_through_links() {
    let mut e = env("newlinks");
    let outside = e.dir.parent().unwrap().join(format!("ved-outside-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, e.dir.join("out")).unwrap();
    std::os::unix::fs::symlink(outside.join("nothing-yet"), e.dir.join("dangling")).unwrap();
    e.ed.tree.rebuild();
    e.panel_on("README.md");
    for bad in ["out/x.txt", "out/", "dangling"] {
        e.keys("a");
        e.keys(bad);
        e.keys("\r");
        assert!(e.ed.msg_err, "{:?}: {:?}", bad, e.ed.msg);
    }
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0, "nothing was made outside the project");
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn d_asks_and_y_deletes_a_file_and_closes_its_buffer() {
    let mut e = env("delfile");
    e.open("src/lib.rs");
    e.open("README.md");
    // the panel follows the current file (README.md): open src to get at lib.rs
    e.panel_on("src");
    e.key(Key::Char('l'));
    e.panel_on("lib.rs");
    e.keys("d");
    assert_eq!(e.cmd_row(), "Delete lib.rs? (y/N)");
    // anything but y is no
    e.keys("n");
    assert!(e.dir.join("src/lib.rs").exists());
    assert!(e.ed.msg.contains("not deleted"));
    assert!(e.ed.prompt.is_none());
    e.keys("d");
    e.key(Key::Enter);
    assert!(e.dir.join("src/lib.rs").exists(), "Enter is not yes");
    e.keys("d");
    e.keys("y");
    assert!(!e.dir.join("src/lib.rs").exists());
    assert!(e.ed.docs.iter().all(|d| d.buf.path.as_ref().is_none_or(|p| !p.ends_with("lib.rs"))), "its buffer is closed");
    assert!(e.ed.msg.contains("deleted lib.rs"), "{:?}", e.ed.msg);
    assert!(!e.ed.tree.rows.iter().any(|r| r.name == "lib.rs"));
    assert!(e.ed.tree.sel < e.ed.tree.rows.len());
    assert!(e.dir.join("README.md").exists());
    // the other buffer is still the current one
    assert!(e.ed.docs[e.ed.cur].buf.path.as_ref().unwrap().ends_with("README.md"));
}

#[test]
fn a_buffer_with_unsaved_changes_outlives_its_deleted_file() {
    let mut e = env("deldirty");
    e.open("src/lib.rs");
    e.keys("ixyz\x1b");
    assert!(e.ed.buf().modified);
    e.panel_on("lib.rs");
    e.keys("dy");
    assert!(!e.dir.join("src/lib.rs").exists());
    assert!(e.ed.msg.contains("unsaved changes kept"), "{:?}", e.ed.msg);
    assert!(e.ed.docs.iter().any(|d| d.buf.modified), "the edits are still there");
}

#[test]
fn dd_asks_and_y_deletes_a_folder_with_everything_in_it() {
    let mut e = env("deldir");
    e.open("src/main.rs");
    e.open("README.md");
    let outside = e.dir.parent().unwrap().join(format!("ved-del-outside-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("precious"), "p").unwrap();
    std::os::unix::fs::symlink(&outside, e.dir.join("src/link-out")).unwrap();
    e.panel_on("src");
    e.keys("d");
    assert!(e.ed.prompt.is_none(), "one d on a folder only says what to do");
    assert!(e.ed.msg.contains("press d again"), "{:?}", e.ed.msg);
    // another key in between: not a dd
    e.keys("jk");
    e.keys("d");
    assert!(e.ed.prompt.is_none());
    e.keys("d");
    let q = e.cmd_row();
    assert!(q.starts_with("Delete the folder src and its ") && q.ends_with("items? (y/N)"), "{:?}", q);
    e.keys("n");
    assert!(e.dir.join("src/main.rs").exists());
    e.keys("dd");
    e.keys("y");
    assert!(!e.dir.join("src").exists());
    assert!(outside.join("precious").exists(), "a link in the folder is removed, never followed");
    assert!(e.ed.docs.iter().all(|d| d.buf.path.as_ref().is_none_or(|p| !p.starts_with(e.dir.join("src")))), "buffers of files in it are closed");
    assert!(e.dir.join("README.md").exists());
    assert!(!e.ed.tree.rows.iter().any(|r| r.name == "src"));
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn noconfirmdelete_deletes_at_once() {
    let mut e = env("delnoconfirm");
    e.keys(":set noconfirmdelete\r");
    e.panel_on("Cargo.toml");
    e.keys("d");
    assert!(!e.dir.join("Cargo.toml").exists(), "d deleted the file");
    assert!(e.ed.prompt.is_none());
    e.panel_on("target");
    e.keys("dd");
    assert!(!e.dir.join("target").exists(), "dd deleted the folder");
    assert!(e.dir.join("src").exists() && e.dir.join("README.md").exists());
}

#[test]
fn a_link_is_deleted_itself_and_what_changed_is_not_deleted() {
    let mut e = env("dellink");
    let outside = e.dir.parent().unwrap().join(format!("ved-link-outside-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("precious"), "p").unwrap();
    std::os::unix::fs::symlink(&outside, e.dir.join("l")).unwrap();
    e.ed.tree.rebuild();
    e.panel_on("l");
    e.keys("dy");
    assert!(std::fs::symlink_metadata(e.dir.join("l")).is_err(), "the link is gone");
    assert!(outside.join("precious").exists(), "its target is not");
    // shown as a file, a folder by the time `y` is pressed
    e.panel_on("README.md");
    e.keys("d");
    std::fs::remove_file(e.dir.join("README.md")).unwrap();
    std::fs::create_dir(e.dir.join("README.md")).unwrap();
    std::fs::write(e.dir.join("README.md/keep"), "k").unwrap();
    e.keys("y");
    assert!(e.ed.msg_err && e.ed.msg.contains("changed"), "{:?}", e.ed.msg);
    assert!(e.dir.join("README.md/keep").exists());
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn the_more_row_and_an_empty_panel_have_nothing_to_delete() {
    let mut e = env("delmore");
    // a folder with more entries than the panel lists
    for i in 0..5100 {
        std::fs::write(e.dir.join(format!("big/f{:05}", i)), "").unwrap_or_else(|_| {
            std::fs::create_dir_all(e.dir.join("big")).unwrap();
            std::fs::write(e.dir.join(format!("big/f{:05}", i)), "").unwrap();
        });
    }
    e.ed.tree.rebuild();
    e.panel_on("big");
    e.key(Key::Char('l'));
    let more = e.ed.tree.rows.iter().position(|r| r.kind == ved::tree::Kind::More).expect("a more row");
    e.ed.tree.select(more);
    e.keys("d");
    assert!(e.ed.prompt.is_none() && e.ed.msg_err, "{:?}", e.ed.msg);
    // `a` there makes the file in that folder
    e.keys("a");
    assert_eq!(e.cmd_row(), "New file in big/:");
    e.keys("\x1b");
}
