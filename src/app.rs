//! The event loop: polls stdin (and later LSP / terminal fds), feeds the editor, renders.
//!
//! It is also the safety net. Every step that runs editor code is guarded: an internal error
//! (a panic) saves the modified buffers to the recovery directory, puts the editor back into a
//! consistent state and carries on. A closed terminal, SIGHUP/SIGTERM, or an I/O error end the
//! session the same way: unsaved work is written to the recovery directory first.

use std::cell::Cell;
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::Options;
use crate::editor::Editor;
use crate::input::InputParser;
use crate::screen::{Frame, Renderer};
use crate::sys;

thread_local! {
    /// True while the main thread runs code whose panics are caught by `guard`.
    static CATCHING: Cell<bool> = const { Cell::new(false) };
}

/// What the last panic said and where (for the message and the recovery log).
static LAST_PANIC: Mutex<Option<String>> = Mutex::new(None);

fn panic_text(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    let msg = payload.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".into());
    match info.location() {
        Some(l) => format!("{} at {}:{}", msg, l.file(), l.line()),
        None => msg,
    }
}

/// Panics we catch must not touch the screen (the editor goes on); any other panic restores the
/// terminal before the usual message is printed.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(mut g) = LAST_PANIC.lock() {
            *g = Some(panic_text(info));
        }
        if CATCHING.with(|c| c.get()) || std::thread::current().name() != Some("main") {
            return;
        }
        sys::restore_tty();
        default_hook(info);
    }));
}

/// Why the loop ended.
enum Exit {
    /// the user quit
    Quit,
    /// SIGHUP, SIGTERM...
    Signal(i32),
    /// stdin or stdout went away (the terminal window was closed)
    TerminalGone,
    /// the editor kept hitting internal errors
    Unstable,
    Error(io::Error),
}

/// Internal errors tolerated within `PANIC_WINDOW` before giving up.
const PANIC_LIMIT: usize = 3;
const PANIC_WINDOW: Duration = Duration::from_secs(10);

/// Run `f` and survive a panic in it: save the unsaved work, repair the editor, say what happened.
/// `Err(())` means the editor is not in a state to continue (too many errors in a row).
fn guard<F: FnOnce(&mut Editor)>(ed: &mut Editor, recent: &mut Vec<Instant>, f: F) -> Result<(), ()> {
    CATCHING.with(|c| c.set(true));
    let r = catch_unwind(AssertUnwindSafe(|| f(ed)));
    CATCHING.with(|c| c.set(false));
    if r.is_ok() {
        return Ok(());
    }
    let what = LAST_PANIC.lock().ok().and_then(|g| g.clone()).unwrap_or_else(|| "internal error".into());
    CATCHING.with(|c| c.set(true));
    let fixed = catch_unwind(AssertUnwindSafe(|| {
        let (saved, failed) = ed.emergency_save();
        crate::recovery::note(&format!("internal error: {}", what));
        ed.repair();
        let short: String = what.chars().take(120).collect();
        if let Some((_, path)) = saved.first() {
            let dir = path.parent().map(|d| d.display().to_string()).unwrap_or_default();
            ed.set_err(format!("internal error ({}); unsaved buffers kept in {}", short, dir));
        } else if !failed.is_empty() {
            ed.set_err(format!("internal error ({}); could NOT save: {}", short, failed.join("; ")));
        } else {
            ed.set_err(format!("internal error ({}); nothing was unsaved", short));
        }
    }));
    CATCHING.with(|c| c.set(false));
    if fixed.is_err() {
        return Err(());
    }
    let now = Instant::now();
    recent.retain(|t| now.duration_since(*t) < PANIC_WINDOW);
    recent.push(now);
    if recent.len() >= PANIC_LIMIT {
        Err(())
    } else {
        Ok(())
    }
}

pub fn run(files: Vec<String>) -> io::Result<()> {
    if !sys::is_tty(0) || !sys::is_tty(1) {
        eprintln!("ved: needs an interactive terminal");
        return Ok(());
    }
    let (opts, warnings) = Options::load();
    let (rows, cols) = sys::term_size();
    // the wake pipe must exist before any language server starts: their reader threads use it
    let wake_fd = sys::create_wake()?;
    let mut ed = Editor::new(opts, rows, cols);
    for w in warnings {
        ed.set_err(w);
    }
    let mut first = true;
    for f in &files {
        let p = ed.expand_path(f);
        match ed.open_file(&p) {
            Ok(i) => {
                if first {
                    ed.switch_to(i);
                    first = false;
                }
            }
            Err(e) => ed.set_err(e),
        }
    }
    let raw = sys::RawMode::enable()?;
    sys::write_stdout(sys::ENTER_SEQ.as_bytes());
    install_panic_hook();
    let exit = event_loop(&mut ed, wake_fd, rows, cols);

    // leaving: unsaved work first (unless the user quit, which already checked), then the servers
    let mut notes: Vec<String> = Vec::new();
    if !matches!(exit, Exit::Quit) {
        CATCHING.with(|c| c.set(true));
        let r = catch_unwind(AssertUnwindSafe(|| ed.emergency_save()));
        CATCHING.with(|c| c.set(false));
        match r {
            Ok((saved, failed)) => {
                for (name, path) in saved {
                    notes.push(format!("unsaved changes to {} were saved as {}", name, path.display()));
                }
                for f in failed {
                    notes.push(format!("COULD NOT save {}", f));
                }
            }
            Err(_) => notes.push("could not save unsaved changes (internal error)".into()),
        }
    }
    CATCHING.with(|c| c.set(true));
    let _ = catch_unwind(AssertUnwindSafe(|| ed.lsp_shutdown()));
    CATCHING.with(|c| c.set(false));
    drop(raw);
    for n in &notes {
        eprintln!("ved: {}", n);
    }
    match exit {
        Exit::Quit => Ok(()),
        Exit::Signal(s) => Err(io::Error::other(format!("stopped by signal {}", s))),
        Exit::TerminalGone => Err(io::Error::other("the terminal went away")),
        Exit::Unstable => Err(io::Error::other("stopping after repeated internal errors")),
        Exit::Error(e) => Err(e),
    }
}

fn event_loop(ed: &mut Editor, wake_fd: i32, rows: usize, cols: usize) -> Exit {
    let mut parser = InputParser::new();
    let mut stdin = sys::RawStdin::new();
    let mut renderer = Renderer::new();
    let mut frame = Frame::new(rows, cols);
    let mut recent: Vec<Instant> = Vec::new();
    // the terminal's own background (the margins around the cell grid) follows the dark theme
    let bg_ok = sys::terminal_takes_bg();
    let mut bg_on = false;
    let mut mouse_on = false;
    let mut buf = [0u8; 4096];
    macro_rules! guarded {
        ($ed:expr, $body:expr) => {
            if guard($ed, &mut recent, $body).is_err() {
                return Exit::Unstable;
            }
        };
    }
    loop {
        if let Some(s) = sys::terminated() {
            return Exit::Signal(s);
        }
        if sys::take_winch() {
            let (r, c) = sys::term_size();
            ed.rows = r;
            ed.cols = c;
            frame = Frame::new(r, c);
            renderer.invalidate();
        }
        let mut out = String::new();
        guarded!(ed, |ed| {
            ed.draw(&mut frame);
            out = renderer.render(&frame);
        });
        let mut bytes = std::mem::take(&mut ed.out_seq).into_bytes();
        let want_bg = bg_ok && ed.opts.theme == crate::config::Theme::Dark && ed.opts.termbg;
        if want_bg != bg_on {
            bg_on = want_bg;
            bytes.extend_from_slice(sys::default_bg_seq(want_bg.then_some(crate::render::THEME_BG_RGB)).as_bytes());
        }
        if ed.opts.mouse != mouse_on {
            mouse_on = ed.opts.mouse;
            bytes.extend_from_slice(if mouse_on { sys::MOUSE_ON_SEQ } else { sys::MOUSE_OFF_SEQ }.as_bytes());
        }
        bytes.extend_from_slice(out.as_bytes());
        if !sys::write_stdout(&bytes) {
            return Exit::TerminalGone;
        }
        if ed.quit {
            return Exit::Quit;
        }
        let mut fds = vec![
            sys::PollFd { fd: 0, events: sys::POLLIN, revents: 0 },
            sys::PollFd { fd: wake_fd, events: sys::POLLIN, revents: 0 },
        ];
        let term_idx = ed.term_fd().map(|fd| {
            fds.push(sys::PollFd { fd, events: sys::POLLIN, revents: 0 });
            fds.len() - 1
        });
        let timeout = if parser.has_pending() { 25 } else { ed.poll_timeout_ms() };
        let n = match sys::poll_fds(&mut fds, timeout) {
            Ok(n) => n,
            Err(e) => return Exit::Error(e),
        };
        if n == 0 {
            while let Some(ev) = parser.next_event(true) {
                guarded!(ed, |ed| ed.handle_event(ev));
            }
            guarded!(ed, |ed| ed.tick());
            continue;
        }
        if fds[1].revents & sys::POLLIN != 0 {
            sys::drain_wake(wake_fd);
        }
        if let Some(i) = term_idx {
            if fds[i].revents & (sys::POLLIN | sys::POLLHUP | sys::POLLERR) != 0 {
                guarded!(ed, |ed| ed.term_readable());
            }
        }
        if fds[0].revents & (sys::POLLIN | sys::POLLHUP | sys::POLLERR | sys::POLLNVAL) != 0 {
            let k = match stdin.read(&mut buf) {
                Ok(0) => return Exit::TerminalGone,
                Ok(k) => k,
                Err(_) => return Exit::TerminalGone,
            };
            parser.push(&buf[..k]);
            while let Some(ev) = parser.next_event(false) {
                guarded!(ed, |ed| ed.handle_event(ev));
                if ed.quit {
                    break;
                }
            }
        }
        guarded!(ed, |ed| ed.tick());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buffer;
    use crate::input::Key;

    fn editor(text: &str) -> Editor {
        let o = Options { lsp: false, ..Options::default() };
        let mut ed = Editor::new(o, 24, 80);
        ed.docs[0].buf = Buffer::from_text(text);
        ed
    }

    #[test]
    fn an_internal_error_saves_unsaved_buffers_repairs_the_editor_and_carries_on() {
        let dir = std::env::temp_dir().join(format!("ved-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("XDG_STATE_HOME", &dir);
        let mut ed = editor("precious\n");
        let mut recent = Vec::new();
        // leave the editor in a messy state, then blow up
        let r = guard(&mut ed, &mut recent, |ed| {
            ed.handle_key(Key::Char('i'));
            ed.handle_key(Key::Char('x'));
            ed.docs[0].buf.begin_group((0, 0));
            ed.docs[0].cursor = (50, 50);
            panic!("boom");
        });
        assert!(r.is_ok(), "one error is survivable");
        // what was typed is on disk, in a private file
        let files: Vec<_> = std::fs::read_dir(dir.join("ved/recovery")).unwrap().flatten().filter(|e| e.path().extension().is_some_and(|x| x == "recovered")).collect();
        assert_eq!(files.len(), 1);
        assert_eq!(std::fs::read_to_string(files[0].path()).unwrap(), "xprecious\n");
        // the editor is consistent and usable again
        assert!(ed.msg.contains("internal error"), "{:?}", ed.msg);
        assert_eq!(ed.mode, crate::editor::Mode::Normal);
        let (r, c) = ed.docs[0].cursor;
        assert!(r < ed.docs[0].buf.line_count() && c <= ed.docs[0].buf.line_len(r));
        ed.handle_key(Key::Char('A'));
        ed.handle_key(Key::Char('!'));
        ed.handle_key(Key::Esc);
        assert_eq!(ed.docs[0].buf.line_string(0), "xprecious!");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_wrecked_document_list_is_rebuilt() {
        let mut ed = editor("a\n");
        let mut recent = Vec::new();
        let r = guard(&mut ed, &mut recent, |ed| {
            ed.docs.clear();
            panic!("boom");
        });
        assert!(r.is_ok());
        assert_eq!(ed.docs.len(), 1);
        assert_eq!(ed.cur, 0);
        ed.handle_key(Key::Char('i'));
        ed.handle_key(Key::Char('z'));
        assert_eq!(ed.docs[0].buf.line_string(0), "z");
    }

    #[test]
    fn repeated_internal_errors_end_the_session_instead_of_looping() {
        let mut ed = editor("a\n");
        let mut recent = Vec::new();
        let mut results = Vec::new();
        for _ in 0..PANIC_LIMIT {
            results.push(guard(&mut ed, &mut recent, |_| panic!("again")));
        }
        assert!(results[..PANIC_LIMIT - 1].iter().all(|r| r.is_ok()));
        assert!(results[PANIC_LIMIT - 1].is_err());
    }
}
