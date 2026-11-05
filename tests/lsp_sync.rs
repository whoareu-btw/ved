#![allow(clippy::field_reassign_with_default)]
//! The language-server client against `tests/fake_nil.py`, a strict stand-in for nil (see that file).
//!
//! The editor runs headless: keys go in through `handle_key`, `tick()` plays the part of the event
//! loop, and the server writes the text it holds to a file that the tests compare with the buffer.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ved::config::Options;
use ved::editor::Editor;
use ved::input::Key;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ved-lspsync-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn keys(s: &str) -> Vec<Key> {
    let cs: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        if cs[i] == '<' {
            if let Some(end) = cs[i..].iter().position(|c| *c == '>') {
                let name: String = cs[i + 1..i + end].iter().collect();
                let k = match name.as_str() {
                    "Esc" => Some(Key::Esc),
                    "CR" => Some(Key::Enter),
                    "BS" => Some(Key::Backspace),
                    "C-r" => Some(Key::Ctrl('r')),
                    _ => None,
                };
                if let Some(k) = k {
                    out.push(k);
                    i += end + 1;
                    continue;
                }
            }
        }
        out.push(Key::Char(cs[i]));
        i += 1;
    }
    out
}

struct Rig {
    ed: Editor,
    out: PathBuf,
    file: PathBuf,
}

fn fake_nil() -> String {
    format!("{}/tests/fake_nil.py", env!("CARGO_MANIFEST_DIR"))
}

/// An editor with the fake nil configured for `.nix`, and `text` open as a file in a fresh directory.
fn rig(name: &str, text: &str) -> Rig {
    let dir = scratch(name);
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let file = dir.join("test.nix");
    std::fs::write(&file, text).unwrap();
    let mut opts = Options::default();
    opts.lsp = true;
    opts.osc52 = false;
    opts.lsp_overrides.push(("nix".into(), Some(format!("python3 {} {}", fake_nil(), out.display()))));
    let mut ed = Editor::new(opts, 24, 80);
    ed.open_file(&file).expect("open");
    Rig { ed, out, file }
}

impl Rig {
    fn feed(&mut self, s: &str) {
        for k in keys(s) {
            self.ed.handle_key(k);
        }
    }

    fn held(&self) -> String {
        std::fs::read_to_string(self.out.join("text")).unwrap_or_default()
    }

    fn dropped(&self) -> bool {
        self.out.join("dropped").exists()
    }

    fn starts(&self) -> usize {
        std::fs::read_to_string(self.out.join("starts")).map_or(0, |s| s.lines().count())
    }

    /// Run the event loop until `f` holds (or fail after a few seconds).
    fn wait(&mut self, what: &str, f: impl Fn(&mut Rig) -> bool) {
        let end = Instant::now() + Duration::from_secs(8);
        loop {
            self.ed.tick();
            if f(self) {
                return;
            }
            if Instant::now() > end {
                panic!("timed out waiting for: {}\nbuffer: {:?}\nserver: {:?}\ndropped: {}\nmessage: {:?}", what, self.ed.buf().text(), self.held(), self.dropped(), self.ed.msg);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Push pending edits to the server and wait until its copy equals the buffer.
    fn settle(&mut self) {
        let idx = self.ed.cur;
        self.ed.lsp_flush(idx);
        self.wait("the server's copy to equal the buffer", |r| {
            let want = r.ed.buf().text().replace('\r', "");
            r.held() == want
        });
        assert!(!self.dropped(), "the server dropped the document as out of sync:\n{}", std::fs::read_to_string(self.out.join("log")).unwrap_or_default());
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.ed.lsp_shutdown();
        let _ = std::fs::remove_dir_all(self.file.parent().unwrap());
    }
}

const NIX: &str = "{ pkgs, ... }:\n{\n  environment.systemPackages = with pkgs; [\n    git\n    vim\n  ];\n}\n";

#[test]
fn the_server_holds_what_the_buffer_holds_after_every_kind_of_edit() {
    let mut r = rig("edits", NIX);
    r.wait("nil to start", |r| r.held() == NIX);
    // every step ends with the server's copy checked; the dangerous ones touch the very end of the
    // file, which nil does not clamp
    let steps = [
        "Gdd",                   // delete the last line
        "Gddgg",                 // ... and again
        "Go<Esc>",               // open a line below the last
        "GA\n  more<Esc>",       // type past the end
        "ggdG",                  // empty the buffer
        "ihello\n<Esc>",         // type into it
        "u",                     // undo
        "<C-r>",                 // redo
        "ggVGd",                 // linewise delete of everything
        "u",                     // back
        "Go\u{1F600} emoji \u{65E5}\u{672C}<Esc>", // astral + CJK
        "ggJ",                   // join
        "Gp",                    // paste at the end
        ":%s/git/GIT/g<CR>",     // substitute
        ":1,2d<CR>",             // ex delete from the top
        ":$d<CR>",               // ex delete the last line
        "yyP",
        "ddp",
        "G$x",
        "ggO<Esc>",
        "3Gdd",
    ];
    for (i, s) in steps.iter().enumerate() {
        r.feed(s);
        // alternate between letting the debounce fire on its own and forcing the flush
        if i % 2 == 0 {
            r.settle();
        } else {
            let idx = r.ed.cur;
            r.ed.lsp_flush(idx);
            r.settle();
        }
    }
}

#[test]
fn a_file_without_a_final_newline_is_synced_in_full() {
    // Buffer::text() leaves the newline out for such a file, so "line N, column 0" does not exist
    // for the server; an incremental change ending there would make nil drop the document.
    let mut r = rig("nonl", "a = 1;\nb = 2;");
    r.wait("nil to start", |r| r.held() == "a = 1;\nb = 2;");
    for s in ["Gddgg", "Go<Esc>", "ggOnew<Esc>", "Gx", "Gp"] {
        r.feed(s);
        r.settle();
    }
}

#[test]
fn crlf_files_reach_the_server_without_carriage_returns() {
    let mut r = rig("crlf", "a = 1;\r\nb = 2;\r\n");
    r.wait("nil to start", |r| r.held() == "a = 1;\nb = 2;\n");
    for s in ["Gddgg", "ggOx<Esc>", "Go<Esc>"] {
        r.feed(s);
        r.settle();
    }
}

#[test]
fn reloading_a_file_that_changed_its_final_newline_keeps_the_server_in_sync() {
    let mut r = rig("reload", "a = 1;\nb = 2;\n");
    r.wait("nil to start", |r| r.held() == "a = 1;\nb = 2;\n");
    // the file loses its final newline behind the editor's back, then is reloaded
    std::fs::write(&r.file, "a = 1;\nb = 2;").unwrap();
    r.feed(":e!<CR>");
    r.settle();
    r.feed("Gddx");
    r.settle();
    std::fs::write(&r.file, "x\ny\nz\n").unwrap();
    r.feed(":e!<CR>");
    r.settle();
    r.feed("Gdd");
    r.settle();
}

#[test]
fn typed_unicode_keeps_utf16_columns_straight() {
    let mut r = rig("utf16", "x = \"\u{1F600}\u{1F600}\";\n");
    r.wait("nil to start", |r| r.held() == "x = \"\u{1F600}\u{1F600}\";\n");
    for s in ["f;i\u{1F468}<Esc>", "0x", "$x", "o\u{65E5}\u{672C}\u{8A9E}<Esc>", "kkdd"] {
        r.feed(s);
        r.settle();
    }
}

#[test]
fn a_document_the_server_forgot_is_sent_again_when_a_request_fails() {
    let mut r = rig("forget", NIX);
    r.wait("nil to start", |r| r.held() == NIX);
    // the server drops the document when it sees the marker (as nil does after a failed change)
    r.feed("GoFORGETME<Esc>");
    let idx = r.ed.cur;
    r.ed.lsp_flush(idx);
    r.wait("the server to drop the document", |r| r.dropped());
    // remove the marker; the server ignores changes for a document it no longer has
    r.feed("dd");
    r.ed.lsp_flush(idx);
    // a hover now fails with "File not loaded"; the client answers by sending the document again
    std::fs::remove_file(r.out.join("dropped")).unwrap();
    r.feed("gg");
    r.feed("K");
    r.wait("the client to re-send the document", |r| r.held() == r.ed.buf().text());
    assert!(!r.dropped());
}

#[test]
fn a_crashing_server_is_restarted_and_gets_its_document_back() {
    let mut r = rig("crash", NIX);
    r.wait("nil to start", |r| r.held() == NIX);
    assert_eq!(r.starts(), 1);
    r.feed("GoCRASHME<Esc>");
    let idx = r.ed.cur;
    r.ed.lsp_flush(idx);
    r.wait("a second server process", |r| r.starts() == 2);
    // remove the trigger; the new process must be in sync with the buffer
    r.feed("dd");
    r.settle();
    assert_eq!(r.starts(), 2, "restarted once, not in a loop");
}

#[test]
fn a_crash_noticed_by_a_failed_write_is_still_handled() {
    let mut r = rig("crash-by-write", NIX);
    r.wait("nil to start", |r| r.held() == NIX);
    r.feed("GoCRASHME<Esc>");
    let idx = r.ed.cur;
    r.ed.lsp_flush(idx);
    // The server dies. Without reading any of its notices, keep editing: the first write that
    // finds the pipe closed ends the writer thread, the next one finds the thread gone. That
    // must not hide the crash from the user or skip the restart.
    std::thread::sleep(Duration::from_millis(400));
    r.feed("ix<Esc>");
    r.ed.lsp_flush(idx);
    std::thread::sleep(Duration::from_millis(150));
    r.feed("ix<Esc>");
    r.ed.lsp_flush(idx);
    r.wait("a second server process", |r| r.starts() == 2);
    assert!(r.ed.msg.contains("lsp"), "{:?}", r.ed.msg);
}

#[test]
fn a_server_that_keeps_crashing_is_given_up_on() {
    let mut r = rig("crashloop", "CRASHME\n");
    // it dies on open, again and again; the client must stop after a few tries, not spin
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        r.ed.tick();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(r.starts() <= 5, "restarted {} times", r.starts());
    assert!(r.ed.msg.contains("lsp"), "{:?}", r.ed.msg);
}

#[test]
fn a_server_that_exits_at_once_is_reported_however_the_exit_is_noticed() {
    // `true` ends before reading anything: our first write may hit a closed pipe or the reader may see
    // the end first. Either way the user must be told, and the editor must not be left with a server
    // that is marked dead but never reported.
    for round in 0..8 {
        let dir = scratch(&format!("exits-at-once-{}", round));
        let file = dir.join("test.nix");
        std::fs::write(&file, NIX).unwrap();
        let mut opts = Options::default();
        opts.lsp = true;
        opts.osc52 = false;
        opts.lsp_overrides.push(("nix".into(), Some("true".into())));
        let mut ed = Editor::new(opts, 24, 80);
        ed.open_file(&file).expect("open");
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end && !ed.msg.contains("exited") {
            ed.tick();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(ed.msg.contains("exited"), "round {}: {:?}", round, ed.msg);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn formatting_that_arrives_after_the_buffer_changed_is_not_applied() {
    let mut r = rig("fmt", "abc\ndef\n");
    r.wait("nil to start", |r| r.held() == "abc\ndef\n");
    r.feed(":format<CR>");
    // the server is slow; the user keeps typing before the answer comes back
    r.feed("Gox<Esc>");
    r.wait("the answer", |r| r.ed.msg.contains("changed while") || r.ed.buf().text().starts_with('A'));
    assert!(r.ed.buf().text().starts_with("abc"), "stale edits were applied: {:?}", r.ed.buf().text());
    // asked again with nothing in between, it is applied
    r.settle();
    r.feed(":format<CR>");
    r.wait("formatting", |r| r.ed.buf().text().starts_with('A'));
}

#[test]
fn rename_that_arrives_after_the_buffer_changed_is_not_applied() {
    let mut r = rig("rename", "abc\ndef\n");
    r.wait("nil to start", |r| r.held() == "abc\ndef\n");
    r.feed(":rename zzz<CR>");
    r.feed("Gox<Esc>");
    r.wait("the answer", |r| r.ed.msg.contains("changed while") || r.ed.buf().text().starts_with('R'));
    assert!(r.ed.buf().text().starts_with("abc"), "stale edits were applied: {:?}", r.ed.buf().text());
}

#[test]
fn startup_requests_from_the_server_are_answered() {
    let mut r = rig("answers", NIX);
    r.wait("nil to start", |r| r.held() == NIX);
    let log = |r: &Rig| std::fs::read_to_string(r.out.join("log")).unwrap_or_default();
    r.wait("the answers to reach the server", |r| {
        let l = log(r);
        l.contains("answer reg-1") && l.contains("answer prog-1") && l.contains("answer cfg-1")
    });
}

#[test]
fn diagnostics_follow_the_text_and_stay_at_the_right_columns() {
    let mut r = rig("diag", "a = 1;\nb = BAD;\n");
    r.wait("a diagnostic", |r| !r.ed.docs[0].diags.is_empty());
    assert_eq!(r.ed.docs[0].diags[0].start, (1, 4));
    // an emoji before the word shifts the UTF-16 column by two, the char column by one
    r.feed("jI\u{1F600}<Esc>");
    r.wait("the diagnostic to move", |r| r.ed.docs[0].diags.first().map(|d| d.start) == Some((1, 5)));
    r.feed("dd");
    r.wait("the diagnostic to go", |r| r.ed.docs[0].diags.is_empty());
}

#[test]
fn nix_completion_keeps_dashes_inside_the_word() {
    let mut r = rig("nixwords", "{\n  x = home-man\n}\n");
    r.wait("nil to start", |r| r.held().contains("home-man"));
    r.feed("jA");
    r.ed.trigger_completion(true);
    r.wait("the popup", |r| r.ed.completion.is_some());
    // the word being completed includes the dash, so the item replaces all of "home-man"
    let c = r.ed.completion.as_ref().unwrap();
    assert_eq!(c.start_col, "  x = ".chars().count());
}

#[test]
fn a_hostile_server_cannot_hang_or_crash_the_editor() {
    // output that is not a language server at all: endless header, absurd length, binary noise
    let dir = scratch("hostile");
    let script = dir.join("evil.py");
    std::fs::write(
        &script,
        r#"
import sys, time
w = sys.stdout.buffer
w.write(b"Content-Length: 99999999999999999\r\n\r\n")
w.flush()
time.sleep(0.2)
w.write(b"X" * 100000)
w.flush()
time.sleep(30)
"#,
    )
    .unwrap();
    let file = dir.join("t.nix");
    std::fs::write(&file, "a = 1;\n").unwrap();
    let mut opts = Options::default();
    opts.lsp = true;
    opts.lsp_overrides.push(("nix".into(), Some(format!("python3 {}", script.display()))));
    let mut ed = Editor::new(opts, 24, 80);
    ed.open_file(&file).unwrap();
    let end = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < end {
        ed.tick();
        std::thread::sleep(Duration::from_millis(10));
    }
    // still alive and responsive
    for k in keys("ihello<Esc>") {
        ed.handle_key(k);
    }
    assert!(ed.buf().text().contains("hello"));
    ed.lsp_shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_server_that_never_reads_does_not_block_the_editor() {
    let dir = scratch("deaf");
    let script = dir.join("deaf.py");
    // answers `initialize`, then never reads another byte
    std::fs::write(
        &script,
        r#"
import sys, json, time
def read_msg():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline().decode().strip()
        if not line:
            break
        k, v = line.split(":", 1)
        headers[k.lower()] = v.strip()
    return json.loads(sys.stdin.buffer.read(int(headers["content-length"])))
m = read_msg()
b = json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": {"capabilities": {"textDocumentSync": 1}}}).encode()
sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(b) + b)
sys.stdout.buffer.flush()
time.sleep(60)
"#,
    )
    .unwrap();
    let file = dir.join("t.nix");
    // a big file: didOpen alone is several times the size of a pipe buffer
    let big = "x = 1;\n".repeat(200_000);
    std::fs::write(&file, &big).unwrap();
    let mut opts = Options::default();
    opts.lsp = true;
    opts.lsp_overrides.push(("nix".into(), Some(format!("python3 {}", script.display()))));
    let mut ed = Editor::new(opts, 24, 80);
    let t = Instant::now();
    ed.open_file(&file).unwrap();
    let end = Instant::now() + Duration::from_millis(800);
    while Instant::now() < end {
        ed.tick();
        for k in keys("ix<Esc>") {
            ed.handle_key(k);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(t.elapsed() < Duration::from_secs(5), "the editor stalled for {:?}", t.elapsed());
    ed.lsp_shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[allow(dead_code)]
fn unused(_: &Path) {}
