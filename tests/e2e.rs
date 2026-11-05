//! End-to-end: run the real binary on a PTY, with our own VT emulator playing the host terminal.

use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ved::screen::Color;
use ved::sys::Pty;
use ved::vt::Vt;

static N: AtomicUsize = AtomicUsize::new(0);
/// `XDG_CONFIG_HOME` is process-wide, so setting it and spawning must not interleave between tests.
static SPAWN: Mutex<()> = Mutex::new(());

fn scratch() -> PathBuf {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("ved-e2e-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&d).unwrap();
    d
}

struct Session {
    pty: Pty,
    vt: Vt,
    dir: PathBuf,
}

impl Drop for Session {
    /// The project folder of a test is removed with its session (a test that fails leaves it for a look
    /// only when `VED_KEEP_TEST_DIRS` is set), so that `cargo test` does not fill /tmp.
    fn drop(&mut self) {
        if std::env::var_os("VED_KEEP_TEST_DIRS").is_none() && self.dir.starts_with(std::env::temp_dir()) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

impl Session {
    /// A session with language servers switched off (the default for tests that are not about LSP).
    fn start(files: &[&str], rows: usize, cols: usize) -> Session {
        Session::start_with(files, rows, cols, "set nolsp\n")
    }

    fn start_with(files: &[&str], rows: usize, cols: usize, config: &str) -> Session {
        Session::start_in(scratch(), files, rows, cols, config)
    }

    /// Like `start_with`, but the editor runs inside `dir` (so relative file names work).
    fn start_in(dir: PathBuf, files: &[&str], rows: usize, cols: usize, config: &str) -> Session {
        let guard = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
        std::fs::create_dir_all(dir.join("cfg/ved")).unwrap();
        // the project panel is tested on its own (see `the_panel_...`): the other tests want the text
        // to start at the left edge, so it is off unless a test's config says something about it
        let config = if config.contains("sidebar") { config.to_string() } else { format!("set nosidebar\n{}", config) };
        std::fs::write(dir.join("cfg/ved/config"), config).unwrap();
        std::env::set_var("XDG_CONFIG_HOME", dir.join("cfg"));
        // where the editor keeps recovery files (see the tests about SIGHUP and a closed terminal)
        std::env::set_var("XDG_STATE_HOME", dir.join("state"));
        std::env::set_var("SHELL", "/bin/sh");
        let args: Vec<String> = files.iter().map(|s| s.to_string()).collect();
        let pty = Pty::spawn(env!("CARGO_BIN_EXE_ved"), &args, rows as u16, cols as u16, Some(&dir)).expect("spawn ved");
        drop(guard);
        let mut s = Session { pty, vt: Vt::new(rows, cols), dir };
        assert!(s.wait(|vt| vt.text().contains("NORMAL")), "editor never drew its status line:\n{}", s.screen());
        s
    }

    fn pump(&mut self) {
        let mut buf = [0u8; 8192];
        loop {
            match (&self.pty.master).read(&mut buf) {
                Ok(0) => break,
                Ok(n) => self.vt.feed(&buf[..n]),
                Err(_) => break,
            }
        }
        if !self.vt.responses.is_empty() {
            let r = std::mem::take(&mut self.vt.responses);
            self.pty.write_all(&r);
        }
    }

    fn wait(&mut self, pred: impl Fn(&Vt) -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(8);
        while Instant::now() < end {
            self.pump();
            if pred(&self.vt) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.pump();
        pred(&self.vt)
    }

    fn wait_secs(&mut self, secs: u64, pred: impl Fn(&Vt) -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            self.pump();
            if pred(&self.vt) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.pump();
        pred(&self.vt)
    }

    fn settle(&mut self) {
        std::thread::sleep(Duration::from_millis(120));
        self.pump();
    }

    fn send(&mut self, s: &str) {
        self.pty.write_all(s.as_bytes());
        self.settle();
    }

    /// A lone Esc must be sent alone, or it would be read as the start of an escape sequence.
    fn esc(&mut self) {
        self.pty.write_all(b"\x1b");
        std::thread::sleep(Duration::from_millis(90));
        self.settle();
    }

    fn screen(&self) -> String {
        self.vt.text()
    }

    fn exited(&mut self) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            self.pump();
            if let Ok(Some(_)) = self.pty.child.try_wait() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

#[test]
fn shows_file_with_line_numbers_and_status() {
    let dir = scratch();
    let f = dir.join("a.txt");
    std::fs::write(&f, "alpha\nbeta\ngamma\n").unwrap();
    let mut s = Session::start(&[f.to_str().unwrap()], 20, 70);
    let scr = s.screen();
    assert!(scr.contains("alpha"), "{}", scr);
    assert!(scr.contains("  2 beta"), "line numbers missing:\n{}", scr);
    assert!(scr.contains("NORMAL"), "{}", scr);
    assert!(scr.contains("a.txt"), "{}", scr);
    assert!(scr.contains("1:1"), "{}", scr);
    s.send(":q\r");
    assert!(s.exited(), "editor did not exit on :q\n{}", s.screen());
}

#[test]
fn edit_and_save_roundtrip() {
    let dir = scratch();
    let f = dir.join("b.txt");
    std::fs::write(&f, "one\ntwo\n").unwrap();
    let mut s = Session::start(&[f.to_str().unwrap()], 20, 70);
    s.send("jA more");
    assert!(s.screen().contains("INSERT"), "{}", s.screen());
    s.esc();
    s.send(":w\r");
    assert!(s.wait(|vt| vt.text().contains("written")), "{}", s.screen());
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "one\ntwo more\n");
    s.send("dd");
    s.send(":q\r");
    assert!(s.screen().contains("No write since last change"), "{}", s.screen());
    s.send(":q!\r");
    assert!(s.exited());
}

#[test]
fn syntax_highlighting_colours_keywords() {
    let dir = scratch();
    let f = dir.join("c.rs");
    std::fs::write(&f, "fn main() {\n    let x = 5; // hi\n}\n").unwrap();
    let mut s = Session::start(&[f.to_str().unwrap()], 20, 70);
    // find the 'fn' cell on the first text row
    let row = (0..20).find(|r| s.vt.row_text(*r).contains("fn main")).expect("row with fn");
    let col = (0..70).find(|c| s.vt.cell(row, *c).ch == 'f' && s.vt.cell(row, *c + 1).ch == 'n').unwrap();
    assert_eq!(s.vt.cell(row, col).style.fg, Color::Idx(176), "keyword colour");
    let row2 = (0..20).find(|r| s.vt.row_text(*r).contains("let x")).unwrap();
    let ccol = (0..70).find(|c| s.vt.cell(row2, *c).ch == '/').unwrap();
    assert_eq!(s.vt.cell(row2, ccol).style.fg, Color::Idx(244), "comment colour");
    s.send(":q\r");
    assert!(s.exited());
}

#[test]
fn fuzzy_file_picker_opens_a_file() {
    let mut s = Session::start(&[], 20, 80);
    // the editor's working directory is the session directory
    std::fs::write(s.dir.join("apple.txt"), "I am apple\n").unwrap();
    std::fs::write(s.dir.join("banana.txt"), "I am banana\n").unwrap();
    s.send("\x10"); // Ctrl-p
    assert!(s.wait(|vt| vt.text().contains("apple.txt") && vt.text().contains("banana.txt")), "{}", s.screen());
    s.send("bnn");
    let scr = s.screen();
    assert!(!scr.contains("apple.txt"), "filter should hide apple:\n{}", scr);
    s.send("\r");
    assert!(s.wait(|vt| vt.text().contains("I am banana")), "{}", s.screen());
    s.send(":q\r");
    assert!(s.exited());
}

#[test]
fn embedded_terminal_runs_commands() {
    let mut s = Session::start(&[], 24, 80);
    s.send("\x1bs"); // Alt-s opens the terminal pane and focuses it
    assert!(s.wait(|vt| vt.text().contains("TERMINAL")), "{}", s.screen());
    s.send("echo hello-$((6*7))\r");
    assert!(s.wait(|vt| vt.text().contains("hello-42")), "command output missing:\n{}", s.screen());
    // the header reflects focus; Alt-s returns to the editor, and typing goes to the editor again
    s.send("\x1bs");
    s.send("ix");
    assert!(s.screen().contains("INSERT"), "{}", s.screen());
    s.esc();
    // the shell keeps running while the editor has focus
    s.send(":term\r");
    s.send("echo second-$((1+1))\r");
    assert!(s.wait(|vt| vt.text().contains("second-2")), "{}", s.screen());
    s.send("exit\r");
    assert!(s.wait(|vt| vt.text().contains("terminal closed") && !vt.text().contains("TERMINAL")), "pane should close when the shell exits:\n{}", s.screen());
    s.send(":q!\r");
    assert!(s.exited());
}

#[test]
fn terminal_keys_alt_s_focuses_alt_q_opens_alt_w_hides_alt_e_closes() {
    let mut s = Session::start(&[], 24, 80);
    // Alt-e and Alt-w with no terminal are harmless
    s.send("\x1be");
    s.send("\x1bw");
    assert!(s.screen().contains("NORMAL"), "{}", s.screen());
    assert!(!s.screen().contains("TERMINAL"));
    // Alt-q opens the pane and focuses it
    s.send("\x1bq");
    assert!(s.wait(|vt| vt.text().contains("TERMINAL") && vt.text().contains("Alt-s editor")), "{}", s.screen());
    assert!(s.screen().contains("Alt-w hide") && s.screen().contains("Alt-e close"), "{}", s.screen());
    s.send("echo alt-$((6*7))\r");
    assert!(s.wait(|vt| vt.text().contains("alt-42")), "typing should reach the shell:\n{}", s.screen());
    // Alt-s gives focus back to the editor (and does not leak into the shell) ...
    s.send("\x1bs");
    assert!(s.wait(|vt| vt.text().contains("Alt-s focus")), "the header should offer to focus the pane:\n{}", s.screen());
    s.send("ix");
    assert!(s.screen().contains("INSERT"), "typing should go to the editor:\n{}", s.screen());
    s.esc();
    assert!(!s.screen().contains("alt-42 x"));
    // ... and once more takes it back; the shell kept running, and its history is still there
    s.send("\x1bs");
    assert!(s.wait(|vt| vt.text().contains("Alt-s editor")), "{}", s.screen());
    s.send("echo again-$((2+2))\r");
    assert!(s.wait(|vt| vt.text().contains("again-4") && vt.text().contains("alt-42")), "{}", s.screen());
    // Alt-w hides the pane from inside the terminal: the editor has the keyboard, the shell lives on
    s.send("\x1bw");
    assert!(s.wait(|vt| !vt.text().contains("TERMINAL")), "the pane should be hidden:\n{}", s.screen());
    s.send("ihidden");
    assert!(s.screen().contains("hidden") && s.screen().contains("INSERT"), "{}", s.screen());
    s.esc();
    // Alt-w again brings it back with its history, focused
    s.send("\x1bw");
    assert!(s.wait(|vt| vt.text().contains("Alt-s editor") && vt.text().contains("again-4")), "{}", s.screen());
    s.send("echo back-$((3+3))\r");
    assert!(s.wait(|vt| vt.text().contains("back-6")), "{}", s.screen());
    // hidden, Alt-q shows it too
    s.send("\x1bw");
    assert!(s.wait(|vt| !vt.text().contains("TERMINAL")), "{}", s.screen());
    s.send("\x1bq");
    assert!(s.wait(|vt| vt.text().contains("Alt-s editor") && vt.text().contains("back-6")), "{}", s.screen());
    // no other key moves focus: Ctrl-\ goes to the shell (a lone Ctrl-\ at a prompt does nothing)
    // and Ctrl-w j / Ctrl-w k are just typed into it
    s.send("\x1c");
    s.send("\x17k");
    s.send("echo still-here\r");
    assert!(s.wait(|vt| vt.text().contains("still-here")), "the terminal should still have focus:\n{}", s.screen());
    assert!(s.screen().contains("Alt-s editor"), "{}", s.screen());
    // Alt-e closes it from inside the terminal, and the editor is back
    s.send("\x1be");
    assert!(s.wait(|vt| !vt.text().contains("TERMINAL") && vt.text().contains("terminal closed")), "{}", s.screen());
    // from the editor: Ctrl-\ and Ctrl-w j no longer open or focus anything
    s.send("\x1c");
    s.send("\x17j");
    assert!(!s.screen().contains("TERMINAL"), "{}", s.screen());
    // Alt-s also works from insert mode; the editor stays in insert mode while the shell has focus
    s.send("i");
    s.send("x");
    s.send("\x1bs");
    assert!(s.wait(|vt| vt.text().contains("TERMINAL")), "{}", s.screen());
    s.send("echo ins-$((1+1))\r");
    assert!(s.wait(|vt| vt.text().contains("ins-2")), "{}", s.screen());
    s.send("\x1bs");
    s.send("y");
    assert!(s.wait(|vt| vt.text().contains("xy")), "{}", s.screen());
    s.send("\x1be");
    assert!(s.wait(|vt| !vt.text().contains("TERMINAL")), "{}", s.screen());
    s.esc();
    s.send(":q!\r");
    assert!(s.exited());
}

/// A terminal session with a roomy pane, whose shell is `shell`.
fn pane_session(shell: &str) -> Session {
    let mut s = Session::start_with(&[], 50, 80, &format!("set nolsp\nset shell={}\nset termheight=80\n", shell));
    s.send("\x1bs");
    assert!(s.wait(|vt| vt.text().contains("TERMINAL")), "{}", s.screen());
    s
}

#[test]
fn home_end_and_friends_reach_the_shell_as_xterm_sends_them() {
    let mut s = pane_session("/bin/sh");
    s.send("cat -v\r");
    // the same keys, as different host terminals report them
    for (label, host) in [
        ("home-csi-H", "\x1b[H"),
        ("home-tilde1", "\x1b[1~"),
        ("home-tilde7", "\x1b[7~"),
        ("home-ss3", "\x1bOH"),
        ("end-csi-F", "\x1b[F"),
        ("end-tilde4", "\x1b[4~"),
        ("end-tilde8", "\x1b[8~"),
        ("end-ss3", "\x1bOF"),
    ] {
        s.send(label);
        s.send(host);
        s.send("\r");
    }
    s.send("del");
    s.send("\x1b[3~");
    s.send("\r");
    s.send("ctrl-left");
    s.send("\x1b[1;5D");
    s.send("\r");
    // cat -v shows each line twice (the tty echoes it, then cat prints it): look for the second one
    let lines = |vt: &Vt, want: &str| vt.text().lines().filter(|l| l.trim_end() == want).count();
    for (label, shown) in [("home-csi-H", "^[[H"), ("home-tilde1", "^[[H"), ("home-tilde7", "^[[H"), ("home-ss3", "^[[H"), ("end-csi-F", "^[[F"), ("end-tilde4", "^[[F"), ("end-tilde8", "^[[F"), ("end-ss3", "^[[F"), ("del", "^[[3~"), ("ctrl-left", "^[[1;5D")] {
        let want = format!("{}{}", label, shown);
        assert!(s.wait(|vt| lines(vt, &want) >= 1), "{} should reach the shell as {}:\n{}", label, shown, s.screen());
    }
    s.send("\x04");
    s.send("\x1be");
    s.send(":q!\r");
    assert!(s.exited());
}

#[test]
fn home_and_end_move_the_cursor_in_a_readline_shell() {
    if !runs("/bin/bash") {
        eprintln!("skipped: no bash");
        return;
    }
    let mut s = pane_session("/bin/bash");
    // wait for the prompt, then type the middle of a command, go to the start and the end of the line
    s.send("echo bc");
    s.send("\x1b[H");
    s.send("a");
    s.send("\x1b[F");
    s.send("d\r");
    assert!(s.wait(|vt| vt.text().lines().any(|l| l.trim_end().ends_with("# aecho bcd"))), "Home/End should have moved the cursor, running a mangled command:\n{}", s.screen());
    s.send("clear\r");
    // the same with the other encodings a host terminal may use
    for (home, end) in [("\x1b[1~", "\x1b[4~"), ("\x1bOH", "\x1bOF"), ("\x1b[7~", "\x1b[8~")] {
        s.send("echo bc");
        s.send(home);
        s.send("X=1;");
        s.send(end);
        s.send("d;echo $X\r");
        assert!(s.wait(|vt| vt.text().lines().any(|l| l.trim_end() == "1")), "{:?} / {:?}:\n{}", home, end, s.screen());
        s.send("clear\r");
    }
    s.send("\x1be");
    s.send(":q!\r");
    assert!(s.exited());
}

/// bash with every binding for Home and End removed: the situation of a shell (zsh with no
/// configuration, for one) that silently drops the xterm sequences.
const UNBIND_HOME_END: &str = r#"bind -r '\e[H'; bind -r '\e[F'; bind -r '\eOH'; bind -r '\eOF'; bind -r '\e[1~'; bind -r '\e[4~'; bind -r '\e[7~'; bind -r '\e[8~'; clear"#;

#[test]
fn home_and_end_work_at_a_prompt_even_when_the_shell_binds_nothing_for_them() {
    if !runs("/bin/bash") {
        eprintln!("skipped: no bash");
        return;
    }
    // with `termkeys=xterm` the keys really are lost: this is the bug, and it proves the unbinding works
    let mut s = Session::start_with(&[], 50, 80, "set nolsp\nset shell=/bin/bash\nset termheight=80\nset termkeys=xterm\n");
    s.send("\x1bs");
    assert!(s.wait(|vt| vt.text().contains("TERMINAL")), "{}", s.screen());
    s.send(&format!("{}\r", UNBIND_HOME_END));
    s.send("echo bc");
    s.send("\x1b[H");
    s.send("a");
    s.send("\x1b[F");
    s.send("d\r");
    assert!(s.wait(|vt| vt.text().lines().any(|l| l.trim_end() == "bcad")), "Home/End should be ignored with termkeys=xterm:\n{}", s.screen());
    s.send("\x1be");
    s.send(":q!\r");
    assert!(s.exited());

    // by default they work at the prompt whatever the shell binds, however the host terminal reports them
    let mut s = pane_session("/bin/bash");
    s.send(&format!("{}\r", UNBIND_HOME_END));
    for (i, (home, end)) in [("\x1b[H", "\x1b[F"), ("\x1b[1~", "\x1b[4~"), ("\x1bOH", "\x1bOF"), ("\x1b[7~", "\x1b[8~")].into_iter().enumerate() {
        s.send("echo bc");
        s.send(home);
        s.send(&format!("A{}=1;", i));
        s.send(end);
        s.send(&format!("d;echo $A{}\r", i));
        assert!(s.wait(|vt| vt.text().lines().any(|l| l.trim_end() == "1")), "Home {:?} / End {:?}:\n{}", home, end, s.screen());
        s.send("clear\r");
    }
    // while a program runs, Home and End are the plain xterm sequences for it
    s.send("cat -v\r");
    s.send("\x1b[H");
    s.send("\x1b[F");
    s.send("\r");
    assert!(s.wait(|vt| vt.text().lines().any(|l| l.trim_end() == "^[[H^[[F")), "{}", s.screen());
    s.send("\x04");
    // and back to the prompt behaviour afterwards
    s.send("clear\r");
    s.send("echo bc");
    s.send("\x1b[H");
    s.send("Z=2;");
    s.send("\x1b[F");
    s.send("d;echo $Z\r");
    assert!(s.wait(|vt| vt.text().lines().any(|l| l.trim_end() == "2")), "{}", s.screen());
    s.send("\x1be");
    s.send(":q!\r");
    assert!(s.exited());
}

#[test]
fn home_and_end_follow_the_shells_application_cursor_mode() {
    if !runs("/bin/bash") {
        eprintln!("skipped: no bash");
        return;
    }
    // readline with `enable-keypad` switches the terminal into application mode (ESC [ ? 1 h) while
    // it edits, then Home and End have to be ESC O H / ESC O F, which is what its terminfo says
    let dir = scratch();
    std::fs::write(dir.join("inputrc"), "set enable-keypad on\n").unwrap();
    let mut s = pane_session("/bin/bash");
    s.send(&format!("exec env INPUTRC={} TERM=xterm-256color bash --noprofile --norc\r", dir.join("inputrc").display()));
    assert!(s.wait(|vt| vt.text().lines().any(|l| l.trim_end().ends_with("bash-5.2#") || l.trim_end().ends_with("bash-5.2$"))), "{}", s.screen());
    s.send("echo bc");
    s.send("\x1b[H");
    s.send("a");
    s.send("\x1b[F");
    s.send("d\r");
    assert!(s.wait(|vt| vt.text().lines().any(|l| l.trim_end().ends_with("# aecho bcd"))), "application-mode Home/End:\n{}", s.screen());
    // and with a program that asks for application mode itself
    s.send("clear; printf '\\033[?1h'; cat -v\r");
    s.send("\x1b[H");
    s.send("\x1b[F");
    s.send("\x1b[A");
    s.send("\r");
    assert!(s.wait(|vt| vt.text().lines().any(|l| l.trim_end() == "^[OH^[OF^[OA")), "{}", s.screen());
    s.send("\x04");
    s.send("printf '\\033[?1l'\r");
    s.send("cat -v\r");
    s.send("\x1b[H");
    s.send("\r");
    assert!(s.wait(|vt| vt.text().lines().any(|l| l.trim_end() == "^[[H")), "back to normal mode:\n{}", s.screen());
    s.send("\x04");
    s.send("\x1be");
    s.send(":q!\r");
    assert!(s.exited());
}

#[test]
fn home_and_end_jump_around_the_scrollback() {
    let mut s = pane_session("/bin/sh");
    s.send("seq 1 200\r");
    assert!(s.wait(|vt| vt.text().contains("200")), "{}", s.screen());
    s.send("\x1b[5~"); // PageUp enters the scrollback
    assert!(s.wait(|vt| vt.text().contains("scrollback")), "{}", s.screen());
    // the title shows [scrollback shown/total]
    let pos = |vt: &Vt| -> Option<(usize, usize)> {
        let t = vt.text();
        let i = t.find("[scrollback ")? + "[scrollback ".len();
        let rest = &t[i..];
        let (a, b) = rest[..rest.find(']')?].split_once('/')?;
        Some((a.parse().ok()?, b.parse().ok()?))
    };
    let (a0, _) = pos(&s.vt).unwrap();
    s.send("\x1b[H"); // Home: the oldest line
    assert!(s.wait(|vt| matches!(pos(vt), Some((a, b)) if a == b && a > a0)), "Home should jump to the top of the history:\n{}", s.screen());
    assert!(s.screen().contains("# seq 1 200"), "{}", s.screen());
    s.send("\x1b[F"); // End: back at the bottom
    assert!(s.wait(|vt| vt.text().contains("200") && vt.text().contains("scrollback 0/")), "{}", s.screen());
    s.send("\x1be");
    s.send(":q!\r");
    assert!(s.exited());
}

#[test]
fn dark_theme_paints_the_whole_screen_and_can_be_turned_off() {
    use ved::render::{THEME_BG, THEME_FG};
    let dir = scratch();
    let f = dir.join("a.txt");
    std::fs::write(&f, "alpha\n").unwrap();
    let mut s = Session::start(&[f.to_str().unwrap()], 20, 70);
    // text, blank cells right of it, the empty rows below and the command line all have our colours
    let (r, c) = (0..20).flat_map(|r| (0..70).map(move |c| (r, c))).find(|&(r, c)| s.vt.cell(r, c).ch == 'a' && s.vt.row_text(r).contains("alpha")).unwrap();
    assert_eq!(s.vt.cell(r, c).style.fg, THEME_FG);
    assert_eq!(s.vt.cell(r, c).style.bg, THEME_BG);
    assert_eq!(s.vt.cell(r, 60).style.bg, THEME_BG);
    assert_eq!(s.vt.cell(10, 30).style.bg, THEME_BG);
    assert_eq!(s.vt.cell(19, 30).style.bg, THEME_BG);
    // the terminal pane's default colours follow the theme too
    s.send("\x1bs");
    assert!(s.wait(|vt| vt.text().contains("TERMINAL")), "{}", s.screen());
    assert_eq!(s.vt.cell(18, 60).style.bg, THEME_BG);
    s.send("\x1be");
    // `:set theme=terminal` hands the colours back to the terminal
    s.send(":set theme=terminal\r");
    assert!(s.wait(|vt| vt.cell(10, 30).style.bg == Color::Default && vt.cell(r, c).style.fg == Color::Default), "{}", s.screen());
    s.send(":set theme=dark\r");
    assert!(s.wait(|vt| vt.cell(10, 30).style.bg == THEME_BG), "{}", s.screen());
    s.send(":q\r");
    assert!(s.exited());
}

#[test]
fn dark_theme_also_recolours_the_terminals_own_background() {
    // the margins around the character grid are painted by the terminal itself, with its default
    // background: ved sets that to the theme colour (OSC 11) and puts it back on exit (OSC 111)
    let mut s = Session::start(&[], 20, 70);
    assert_eq!(s.vt.default_bg.as_deref(), Some("rgb:1c/1c/1c"), "{}", s.screen());
    s.send(":set theme=terminal\r");
    assert!(s.wait(|vt| vt.default_bg.is_none()), "terminal colours should be handed back");
    s.send(":set theme=dark\r");
    assert!(s.wait(|vt| vt.default_bg.is_some()));
    s.send(":set notermbg\r");
    assert!(s.wait(|vt| vt.default_bg.is_none()), "notermbg leaves the terminal's background alone");
    s.send(":set termbg\r");
    assert!(s.wait(|vt| vt.default_bg.is_some()));
    s.send(":q\r");
    assert!(s.exited());
    s.pump();
    assert!(s.vt.default_bg.is_none(), "the terminal's background must be reset on exit");
}

#[test]
fn terminal_pane_resizes_with_the_window() {
    let mut s = Session::start(&[], 24, 80);
    s.send("\x1bs");
    assert!(s.wait(|vt| vt.text().contains("TERMINAL")));
    s.send("stty size\r");
    assert!(s.wait(|vt| {
        let t = vt.text();
        t.lines().any(|l| l.split_whitespace().count() == 2 && l.trim().ends_with(" 80"))
    }), "{}", s.screen());
    s.send("\x1bs");
    s.send(":q!\r");
    assert!(s.exited());
}

// ---- language server client ----------------------------------------------------------------

fn wait_until(f: impl Fn() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(8);
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    f()
}

fn read(p: &std::path::Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

#[test]
fn lsp_features_against_a_scripted_server() {
    let dir = scratch();
    std::fs::write(dir.join("code.fake"), "def alpha_item():\n    return 1\nx = alpha_item() BAD\n").unwrap();
    let view = dir.join("server-view.txt");
    let cfg = format!("lsp fake python3 {}/tests/fake_lsp.py {}\n", env!("CARGO_MANIFEST_DIR"), view.display());
    let mut s = Session::start_in(dir.clone(), &["code.fake"], 24, 100, &cfg);

    // diagnostics arrive: count in the status line, message when the cursor is on the line
    assert!(s.wait(|vt| vt.text().contains("E1")), "no diagnostics:\n{}", s.screen());
    s.send("G");
    assert!(s.wait(|vt| vt.text().contains("[fake] bad word found")), "{}", s.screen());
    s.send("gg");
    s.send("]d");
    assert!(s.wait(|vt| vt.text().contains("3:18")), "]d should jump to the error:\n{}", s.screen());

    // hover
    s.send("0fa");
    s.send("K");
    assert!(s.wait(|vt| vt.text().contains("word: alpha_item") && vt.text().contains("fake docs")), "{}", s.screen());
    s.esc();
    assert!(!s.screen().contains("fake docs"), "popup should close on Esc");

    // go to definition, then references through the picker
    s.send("G0fa");
    s.send("gd");
    assert!(s.wait(|vt| vt.text().contains("1:5")), "gd should land on the def:\n{}", s.screen());
    s.send("gr");
    assert!(s.wait(|vt| vt.text().contains("code.fake:3:5")), "references picker:\n{}", s.screen());
    s.send("BAD");
    s.send("\r");
    assert!(s.wait(|vt| vt.text().contains("3:5")), "{}", s.screen());

    // incremental sync keeps the server's copy identical to the buffer
    s.send("onew_line_here");
    s.esc();
    let want = "def alpha_item():\n    return 1\nx = alpha_item() BAD\nnew_line_here\n";
    assert!(wait_until(|| read(&view) == want), "server copy after `o`: {:?}", read(&view));
    s.send("ggA   ");
    s.esc();
    let spaced = "def alpha_item():   \n    return 1\nx = alpha_item() BAD\nnew_line_here\n";
    assert!(wait_until(|| read(&view) == spaced), "server copy after A: {:?}", read(&view));
    s.send(":format\r");
    assert!(wait_until(|| read(&view) == want), "format should trim and sync: {:?}", read(&view));
    s.send("ddu");
    assert!(wait_until(|| read(&view) == want), "undo should sync: {:?}", read(&view));

    // rename touches both occurrences
    s.send("gg0fa");
    s.send(":rename gamma_item\r");
    assert!(wait_until(|| read(&view).contains("def gamma_item():") && read(&view).contains("x = gamma_item() BAD")), "{:?}", read(&view));

    // completion popup while typing: Tab moves down (the popup stays), Shift-Tab moves back up,
    // Enter accepts the highlighted option instead of inserting a newline
    s.send("Go");
    s.send("alp");
    assert!(s.wait(|vt| vt.text().contains("alpine")), "completion popup:\n{}", s.screen());
    s.send("\t");
    assert!(s.screen().contains("alpine"), "Tab must move the highlight, not accept:\n{}", s.screen());
    s.send("\x1b[Z"); // Shift-Tab
    s.send("\r");
    assert!(!s.screen().contains("alpine"), "Enter should accept and close the popup:\n{}", s.screen());
    // a second list: Shift-Tab from the top wraps to the last option
    s.esc();
    s.send("o");
    s.send("alp");
    assert!(s.wait(|vt| vt.text().contains("alpine")), "second popup:\n{}", s.screen());
    s.send("\x1b[Z");
    s.send("\r");
    s.esc();
    s.send(":w\r");
    assert!(s.wait(|vt| vt.text().contains("written")), "{}", s.screen());
    let saved = read(&dir.join("code.fake"));
    assert_eq!(saved, "def gamma_item():\n    return 1\nx = gamma_item() BAD\nnew_line_here\nalpha_item\nalpine\n");

    // :lsp lists the running server
    s.send(":lsp\r");
    assert!(s.wait(|vt| vt.text().contains("[ready]") && vt.text().contains("completion")), "{}", s.screen());
    s.esc();

    s.send(":q\r");
    assert!(s.exited());
    // the server was asked to exit cleanly
    assert!(wait_until(|| dir.join("server-view.txt.exit").exists()), "server never got `exit`");
}

#[test]
fn lsp_snippet_completions_expand_with_placeholders() {
    let dir = scratch();
    std::fs::write(dir.join("snip.fake"), "BAD\n").unwrap();
    let view = dir.join("server-view.txt");
    let cfg = format!("lsp fake python3 {}/tests/fake_lsp.py {}\n", env!("CARGO_MANIFEST_DIR"), view.display());
    let mut s = Session::start_in(dir.clone(), &["snip.fake"], 24, 100, &cfg);
    assert!(s.wait(|vt| vt.text().contains("E1")), "server never became ready:\n{}", s.screen());

    // the bug report: `#inc`, Tab to `include <header>`, Enter -> the whole directive, not `#include`
    s.send("Go");
    s.send("#inc");
    assert!(s.wait(|vt| vt.text().contains("include <header>")), "completion popup:\n{}", s.screen());
    s.send("\t");
    s.send("\r");
    assert!(s.wait(|vt| vt.text().contains("#include <header>")), "snippet text missing:\n{}", s.screen());
    assert!(!s.screen().contains("include \"header\""), "popup should be closed:\n{}", s.screen());
    // `header` is highlighted as the placeholder (the selection background), its neighbours are not
    let row = (0..24).find(|r| s.vt.row_text(*r).contains("#include <header>")).unwrap();
    let col = s.vt.row_text(row).find("header").unwrap();
    let sel = Color::Idx(238);
    assert_eq!(s.vt.cell(row, col).style.bg, sel, "placeholder not highlighted");
    assert_eq!(s.vt.cell(row, col + 5).style.bg, sel);
    assert_ne!(s.vt.cell(row, col - 1).style.bg, sel);
    assert_ne!(s.vt.cell(row, col + 6).style.bg, sel);
    // typing replaces it; Tab steps out after the `>`
    s.send("stdio.h");
    assert!(s.wait(|vt| vt.text().contains("#include <stdio.h>")), "{}", s.screen());
    s.send("\t");
    s.send("\rint x;");
    s.esc();

    // a function-call snippet: both arguments are filled in one after the other (and no popup comes up
    // on its own while doing so, because Tab belongs to the placeholders then)
    s.send("o");
    s.send("snipc");
    assert!(s.wait(|vt| vt.text().contains("snipcall(int a, int b)")), "{}", s.screen());
    s.send("\r");
    assert!(s.wait(|vt| vt.text().contains("snipcall(a, b)")), "{}", s.screen());
    s.send("al");
    assert!(!s.screen().contains("alpha_item"), "no popup while a placeholder is being filled:\n{}", s.screen());
    s.send("\t");
    s.send("be");
    s.send("\t");
    s.send(";");
    s.esc();
    s.send(":w\r");
    assert!(s.wait(|vt| vt.text().contains("written")), "{}", s.screen());
    assert_eq!(read(&dir.join("snip.fake")), "BAD\n#include <stdio.h>\nint x;\nsnipcall(al, be);\n");
    // the server's copy followed every edit
    assert!(wait_until(|| read(&view) == "BAD\n#include <stdio.h>\nint x;\nsnipcall(al, be);\n"), "server copy: {:?}", read(&view));

    s.send(":q\r");
    assert!(s.exited());
}

#[test]
fn lsp_include_headers_like_clangd() {
    // clangd's `#include` flow: the directive comes with empty `<>`, the header names (which carry their
    // own closing `>`) come from a second popup. The bracket must not end up doubled.
    let dir = scratch();
    std::fs::write(dir.join("inc.fake"), "BAD\n").unwrap();
    let view = dir.join("server-view.txt");
    let cfg = format!("lsp fake python3 {}/tests/fake_lsp.py {}\n", env!("CARGO_MANIFEST_DIR"), view.display());
    let mut s = Session::start_in(dir.clone(), &["inc.fake"], 24, 100, &cfg);
    assert!(s.wait(|vt| vt.text().contains("E1")), "server never became ready:\n{}", s.screen());

    s.send("Go");
    s.send("#include_n");
    assert!(s.wait(|vt| vt.text().contains("include_next <header>")), "directive popup:\n{}", s.screen());
    s.send("\r");
    assert!(s.wait(|vt| vt.text().contains("#include_next <>")), "directive text:\n{}", s.screen());
    s.send("std");
    assert!(s.wait(|vt| vt.text().contains("stdio.h>") && vt.text().contains("stdlib.h>")), "header popup:\n{}", s.screen());
    s.send("\r");
    assert!(s.wait(|vt| vt.text().contains("#include_next <stdio.h>")), "{}", s.screen());
    assert!(!s.screen().contains("<stdio.h>>"), "closing bracket doubled:\n{}", s.screen());
    s.esc();

    // a directory: picking `sys/` lists what is inside it, and the file there is merged the same way
    s.send("o");
    s.send("#include <sy");
    assert!(s.wait(|vt| vt.text().contains("sys/")), "directory popup:\n{}", s.screen());
    s.send("\r");
    assert!(s.wait(|vt| vt.text().contains("types.h>")), "contents of sys/ should be listed:\n{}", s.screen());
    s.send("\r");
    assert!(s.wait(|vt| vt.text().contains("#include <sys/stat.h>")), "{}", s.screen());
    assert!(!s.screen().contains("stat.h>>"), "{}", s.screen());
    s.esc();

    s.send(":w\r");
    assert!(s.wait(|vt| vt.text().contains("written")), "{}", s.screen());
    let saved = read(&dir.join("inc.fake"));
    assert_eq!(saved, "BAD\n#include_next <stdio.h>\n#include <sys/stat.h>\n");

    s.send(":q!\r");
    assert!(s.exited());
}

#[test]
fn lsp_missing_server_is_reported() {
    let dir = scratch();
    std::fs::write(dir.join("a.fake"), "hello\n").unwrap();
    let mut s = Session::start_in(dir, &["a.fake"], 20, 90, "lsp fake no-such-language-server-xyz\n");
    assert!(s.wait(|vt| vt.text().contains("not found in PATH")), "{}", s.screen());
    // the editor stays fully usable, and completion falls back to words from the buffer
    s.send("o");
    s.send("hel");
    s.send("\x0e"); // Ctrl-n
    assert!(s.wait(|vt| vt.text().contains("hello") && vt.text().contains("buffer")), "{}", s.screen());
    s.esc();
    s.send(":q!\r");
    assert!(s.exited());
}

/// True when `prog --version` runs successfully (a rustup proxy without the component does not).
fn runs(prog: &str) -> bool {
    match ved::lsp::which(prog) {
        Some(p) => std::process::Command::new(p).arg("--version").output().map(|o| o.status.success()).unwrap_or(false),
        None => false,
    }
}

/// Talks to a real rust-analyzer when a working one is installed (skipped otherwise).
#[test]
fn lsp_real_rust_analyzer() {
    if !runs("rust-analyzer") {
        eprintln!("skipping: no working rust-analyzer");
        return;
    }
    let dir = scratch();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() {\n    let x: i32 = \"hello\";\n}\n").unwrap();
    let mut s = Session::start_in(dir, &["src/main.rs"], 24, 100, "");
    assert!(s.wait_secs(120, |vt| vt.text().contains("E1")), "rust-analyzer never reported the type error:\n{}", s.screen());
    s.send("w");
    s.send("K");
    assert!(s.wait_secs(20, |vt| vt.text().contains("fn main")), "hover:\n{}", s.screen());
    s.esc();
    s.send(":q!\r");
    assert!(s.exited());
}

/// Talks to a real pyright when installed (skipped otherwise).
#[test]
fn lsp_real_pyright() {
    if ved::lsp::which("pyright-langserver").is_none() || ved::lsp::which("node").is_none() {
        eprintln!("skipping: pyright-langserver or node not installed");
        return;
    }
    let dir = scratch();
    std::fs::write(dir.join("demo.py"), "def greet(name: str) -> str:\n    return 'hi ' + name\n\nx: int = 'hello'\ngreet(1)\n").unwrap();
    let mut s = Session::start_in(dir, &["demo.py"], 24, 110, "");
    assert!(s.wait_secs(90, |vt| vt.text().contains("E2")), "pyright never reported its two errors:\n{}", s.screen());
    // hover on `greet` in the definition shows the signature
    s.send("wK");
    assert!(s.wait_secs(20, |vt| vt.text().contains("def greet")), "hover:\n{}", s.screen());
    s.esc();
    // go to definition from the call on the last line
    s.send("G");
    s.send("gd");
    assert!(s.wait_secs(20, |vt| vt.text().contains("1:5")), "gd:\n{}", s.screen());
    // completion after a dot
    s.send("Go");
    s.send("x.bit_");
    assert!(s.wait_secs(20, |vt| vt.text().contains("bit_length")), "completion:\n{}", s.screen());
    s.esc();
    s.send(":q!\r");
    assert!(s.exited());
}

fn recovered_files(dir: &std::path::Path) -> Vec<(PathBuf, String)> {
    let rec = dir.join("state/ved/recovery");
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&rec) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "recovered") {
                let text = std::fs::read_to_string(&p).unwrap_or_default();
                out.push((p, text));
            }
        }
    }
    out
}

fn wait_for_recovered(dir: &std::path::Path, needle: &str) -> Option<(PathBuf, String)> {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if let Some(f) = recovered_files(dir).into_iter().find(|(_, t)| t.contains(needle)) {
            return Some(f);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

#[test]
fn sigterm_saves_unsaved_work_to_the_recovery_directory_and_leaves_the_file_alone() {
    let dir = scratch();
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    let mut s = Session::start_in(dir.clone(), &["a.txt"], 24, 80, "set nolsp\n");
    s.send("ihello ");
    s.esc();
    assert!(s.wait(|vt| vt.text().contains("[+]") || vt.text().contains("+")), "{}", s.screen());
    let pid = s.pty.child.id().to_string();
    std::process::Command::new("kill").args(["-TERM", &pid]).status().unwrap();
    assert!(s.exited(), "ved did not exit on SIGTERM");
    let (path, text) = wait_for_recovered(&dir, "hello").expect("no recovery file was written");
    assert_eq!(text, "hello one\n");
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\n", "the original must not be touched");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
}

#[test]
fn closing_the_terminal_window_saves_unsaved_work() {
    let dir = scratch();
    std::fs::write(dir.join("b.txt"), "base\n").unwrap();
    let mut s = Session::start_in(dir.clone(), &["b.txt"], 24, 80, "set nolsp\n");
    s.send("Gotyped before the window closed");
    s.esc();
    // the terminal goes away: the master side of the pty is closed, the editor gets SIGHUP / EIO
    let gone = std::mem::replace(&mut s.pty.master, std::fs::File::open("/dev/null").unwrap());
    drop(gone);
    assert!(s.exited(), "ved did not notice that its terminal was closed");
    let (_, text) = wait_for_recovered(&dir, "typed before").expect("no recovery file was written");
    assert_eq!(text, "base\ntyped before the window closed\n");
    assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "base\n");
}

#[test]
fn a_clean_buffer_leaves_no_recovery_file() {
    let dir = scratch();
    std::fs::write(dir.join("c.txt"), "x\n").unwrap();
    let mut s = Session::start_in(dir.clone(), &["c.txt"], 24, 80, "set nolsp\n");
    let pid = s.pty.child.id().to_string();
    std::process::Command::new("kill").args(["-TERM", &pid]).status().unwrap();
    assert!(s.exited());
    assert!(recovered_files(&dir).is_empty());
}

#[test]
fn saving_replaces_the_file_atomically_keeps_its_mode_and_follows_symlinks() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch();
    let real = dir.join("real.txt");
    std::fs::write(&real, "old\n").unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o640)).unwrap();
    std::os::unix::fs::symlink(&real, dir.join("link.txt")).unwrap();
    let mut s = Session::start_in(dir.clone(), &["link.txt"], 24, 80, "set nolsp\n");
    s.send("ccnew text");
    s.esc();
    s.send(":w\r");
    assert!(s.wait(|vt| vt.text().contains("written")), "{}", s.screen());
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "new text\n");
    assert!(std::fs::symlink_metadata(dir.join("link.txt")).unwrap().file_type().is_symlink(), "the symlink must stay a symlink");
    assert_eq!(std::fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o640);
    // no temporary files left behind
    let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).collect();
    assert!(leftovers.is_empty(), "{:?}", leftovers);
    s.send(":q\r");
    assert!(s.exited());
}

#[test]
fn a_file_changed_behind_the_editors_back_is_not_overwritten_without_a_bang() {
    let dir = scratch();
    std::fs::write(dir.join("d.txt"), "mine\n").unwrap();
    let mut s = Session::start_in(dir.clone(), &["d.txt"], 24, 90, "set nolsp\n");
    // someone else rewrites the file (a different length, so the change is certain to show)
    std::fs::write(dir.join("d.txt"), "theirs, changed meanwhile\n").unwrap();
    s.send("ccedited");
    s.esc();
    s.send(":w\r");
    assert!(s.wait(|vt| vt.text().contains("changed on disk")), "{}", s.screen());
    assert_eq!(std::fs::read_to_string(dir.join("d.txt")).unwrap(), "theirs, changed meanwhile\n");
    s.send(":w!\r");
    assert!(s.wait(|vt| vt.text().contains("written")), "{}", s.screen());
    assert_eq!(std::fs::read_to_string(dir.join("d.txt")).unwrap(), "edited\n");
    s.send(":q\r");
    assert!(s.exited());
}

#[test]
fn a_command_that_never_ends_cannot_hang_the_editor_forever_but_output_is_capped() {
    let dir = scratch();
    std::fs::write(dir.join("e.txt"), "x\n").unwrap();
    let mut s = Session::start_in(dir.clone(), &["e.txt"], 24, 90, "set nolsp\n");
    // endless output: stopped at the output cap, the editor is back at once
    s.send(":r !yes\r");
    assert!(s.wait_secs(20, |vt| vt.text().contains("more than")), "{}", s.screen());
    s.send(":q!\r");
    assert!(s.exited());
}

// ---- the project panel and the mouse, on a real terminal -----------------------------------------

impl Session {
    /// A left click at (row, col), 0-based, as an SGR mouse report.
    fn click(&mut self, row: usize, col: usize) {
        self.send(&format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", col + 1, row + 1, col + 1, row + 1));
    }

    fn wheel(&mut self, row: usize, col: usize, up: bool) {
        self.send(&format!("\x1b[<{};{};{}M", if up { 64 } else { 65 }, col + 1, row + 1));
    }

    /// The screen row whose text (after the first `skip` columns) contains `what`.
    fn row_with(&self, what: &str) -> Option<usize> {
        (0..self.vt.rows).find(|&r| self.vt.row_text(r).contains(what))
    }
}

fn project_dir() -> PathBuf {
    let dir = scratch();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() {\n    println!(\"hi\");\n}\n").unwrap();
    std::fs::write(dir.join("README.md"), "# the readme\n").unwrap();
    std::fs::write(dir.join("notes.txt"), "some notes\n").unwrap();
    dir
}

#[test]
fn the_panel_is_there_and_the_keyboard_opens_files_from_it() {
    let dir = project_dir();
    let mut s = Session::start_in(dir, &["notes.txt"], 24, 100, "set nolsp\nset sidebar\n");
    let scr = s.screen();
    // the bar, with the button, the project name and the tab; the panel's entries; the text
    let first = scr.lines().next().unwrap().to_string();
    assert!(first.starts_with(" \u{2261} ") && first.contains("notes.txt"), "{}", scr);
    assert!(scr.contains("src") && scr.contains("README.md"), "{}", scr);
    assert!(scr.contains("some notes"), "{}", scr);
    // Ctrl-w h puts the keyboard in the panel: gg j l unfolds src, j Enter opens main.rs
    s.send("\x17h");
    assert!(s.wait(|vt| vt.text().contains("TREE")), "{}", s.screen());
    s.send("ggjl");
    assert!(s.wait(|vt| vt.text().contains("main.rs")), "{}", s.screen());
    s.send("j\r");
    assert!(s.wait(|vt| vt.text().contains("println")), "{}", s.screen());
    assert!(s.screen().contains("NORMAL") && !s.screen().contains("TREE"), "{}", s.screen());
    // G goes to the last entry, README.md
    s.send("\x17h");
    s.send("G\r");
    assert!(s.wait(|vt| vt.text().contains("the readme")), "{}", s.screen());
    // Alt-b hides the panel; the text moves left; Alt-b shows it again
    s.send("\x1bb");
    assert!(s.wait(|vt| (0..vt.rows).any(|r| vt.row_text(r).trim_start().starts_with("1 # the readme"))), "{}", s.screen());
    s.send("\x1bb");
    assert!(s.wait(|vt| vt.text().contains("TREE")), "{}", s.screen());
    s.send("\x1b");
    s.send(":q!\r");
    assert!(s.exited());
}

#[test]
fn the_mouse_works_on_a_real_terminal_and_is_given_back_on_exit() {
    let dir = project_dir();
    let mut s = Session::start_in(dir, &["notes.txt"], 24, 100, "set nolsp\nset sidebar\n");
    // ved asked the terminal for SGR mouse reports
    assert!(s.vt.mouse && s.vt.mouse_sgr, "mouse reporting should be on");
    // a click on the src row opens the directory, a click on main.rs opens the file
    let r = s.row_with("src").expect("src row");
    s.click(r, 4);
    assert!(s.wait(|vt| vt.text().contains("main.rs")), "{}", s.screen());
    let r = s.row_with("main.rs").expect("main.rs row");
    s.click(r, 8);
    assert!(s.wait(|vt| vt.text().contains("println")), "{}", s.screen());
    // a click in the text moves the cursor: the status line shows the position
    let r = s.row_with("println").expect("text row");
    // (a column, not a byte offset: the indent guide `│` in front of it is three bytes long)
    let line = s.vt.row_text(r);
    let c = line[..line.find("println").unwrap()].chars().count();
    s.click(r, c + 3);
    assert!(s.wait(|vt| vt.text().contains("2:8 ")), "cursor should be at line 2, column 8 (the `n` of println, after four spaces):\n{}", s.screen());
    // the button hides the panel, and brings it back
    s.click(0, 1);
    assert!(s.wait(|vt| !vt.text().contains("README.md")), "{}", s.screen());
    s.click(0, 1);
    assert!(s.wait(|vt| vt.text().contains("README.md")), "{}", s.screen());
    // a tab click switches buffers
    let first = s.vt.row_text(0);
    let c = first[..first.find("notes.txt").expect("tab for notes.txt")].chars().count();
    s.click(0, c + 2);
    assert!(s.wait(|vt| vt.text().contains("some notes")), "{}", s.screen());
    // the tabs have an x; the panel's edge can be dragged (the edge is column 30, 1-based)
    assert!(s.vt.row_text(0).contains('\u{d7}'), "{}", s.screen());
    let r = s.row_with("README.md").expect("a panel row");
    s.send(&format!("\x1b[<0;30;{}M\x1b[<32;38;{}M\x1b[<32;45;{}M\x1b[<0;45;{}m", r + 1, r + 1, r + 1, r + 1));
    assert!(s.wait(|vt| vt.cell(r, 44).ch == '\u{2502}'), "the panel should now be 45 columns wide:\n{}", s.screen());
    assert_ne!(s.vt.cell(r, 29).ch, '\u{2502}');
    // the x of a tab closes it; closing the last one leaves an empty buffer
    let tabs = s.vt.row_text(0).chars().filter(|c| *c == '\u{d7}').count();
    assert_eq!(tabs, 2, "{}", s.screen());
    for left in [1, 0] {
        let x = s.vt.row_text(0).chars().position(|c| c == '\u{d7}').unwrap();
        s.click(0, x);
        assert!(s.wait(|vt| vt.row_text(0).chars().filter(|c| *c == '\u{d7}').count() == 1 && (left > 0 || vt.row_text(0).contains("[No Name]"))), "{}", s.screen());
    }
    // a stray mouse report in the command line is not typed into it
    s.send(":se");
    s.send("\x1b[<0;50;10M\x1b[<0;50;10m");
    s.send("t\r");
    assert!(s.screen().contains("NORMAL"), "{}", s.screen());
    // :set nomouse turns the reports off, :set mouse on again
    s.send(":set nomouse\r");
    assert!(s.wait(|vt| !vt.mouse && !vt.mouse_sgr), "reporting should be off");
    s.send(":set mouse\r");
    assert!(s.wait(|vt| vt.mouse && vt.mouse_sgr && vt.mouse_motion));
    // leaving gives the mouse back to the terminal
    s.send(":q!\r");
    assert!(s.exited());
    s.pump();
    assert!(!s.vt.mouse && !s.vt.mouse_sgr && !s.vt.mouse_motion, "mouse reporting must be off after exit");
}

#[test]
fn the_wheel_and_clicks_reach_a_program_in_the_terminal_pane() {
    let dir = project_dir();
    let mut s = Session::start_in(dir, &["notes.txt"], 40, 100, "set nolsp\nset sidebar\nset termheight=60\n");
    s.send("\x1bq");
    assert!(s.wait(|vt| vt.text().contains("TERMINAL")), "{}", s.screen());
    // a program that turns on SGR mouse reporting and shows what it receives
    s.send("printf '\\033[?1000h\\033[?1006h'; cat -v\r");
    s.settle();
    let pane = s.row_with("cat -v").expect("pane row") + 2;
    s.click(pane, 50);
    assert!(s.wait(|vt| vt.text().contains("^[[<0;")), "the click should reach the program:\n{}", s.screen());
    s.wheel(pane, 50, true);
    assert!(s.wait(|vt| vt.text().contains("^[[<64;")), "the wheel should reach the program:\n{}", s.screen());
    // the coordinates are relative to the pane, not to the screen
    assert!(!s.screen().contains(&format!("^[[<0;{};{}M", 51, pane + 1)), "{}", s.screen());
    // this program asked for clicks only: a drag (the button held while the mouse moves) is not sent to it
    s.send(&format!("\x1b[<0;50;{}M\x1b[<32;60;{}M\x1b[<0;60;{}m", pane + 1, pane + 1, pane + 1));
    assert!(s.wait(|vt| vt.text().matches("^[[<0;").count() >= 4), "the press and release arrive:\n{}", s.screen());
    assert!(!s.screen().contains("^[[<32;"), "no motion for a program that did not ask:\n{}", s.screen());
    // one that asked for it (mode 1002) gets the motion too
    s.send("\x03");
    s.send("printf '\\033[?1000l\\033[?1002h\\033[?1006h'; cat -v\r");
    s.settle();
    s.send(&format!("\x1b[<0;50;{}M\x1b[<32;60;{}M\x1b[<0;60;{}m", pane + 1, pane + 1, pane + 1));
    assert!(s.wait(|vt| vt.text().contains("^[[<32;")), "motion for a program that asked for it:\n{}", s.screen());
    s.send("\x03");
    s.send("printf '\\033[?1002l\\033[?1006l'\r");
    s.send("\x1be");
    s.send(":q!\r");
    assert!(s.exited());
}
