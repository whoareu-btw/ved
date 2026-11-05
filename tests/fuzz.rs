#![allow(clippy::field_reassign_with_default)]
//! Dependency-free fuzz / stress tests.
//!
//! Everything is seeded and deterministic, so a failure prints the seed (and, for the editor, the
//! keys that led there) and can be replayed. Every case runs on its own thread under a watchdog, so
//! a hang is reported as a failure instead of freezing the test run.
//!
//! `VED_FUZZ=<n>` multiplies the amount of work (the default is a quick pass; try `VED_FUZZ=50` for
//! a long soak).

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ved::buffer::Buffer;
use ved::config::Options;
use ved::editor::Editor;
use ved::input::{Event, InputParser, Key, Mouse};
use ved::json::Json;
use ved::screen::{Frame, Renderer};
use ved::vt::Vt;

// ---- tools ---------------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03 | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
    /// inclusive
    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo + 1)
    }
    fn chance(&mut self, num: usize, den: usize) -> bool {
        self.below(den) < num
    }
    fn pick<'a, T>(&mut self, s: &'a [T]) -> &'a T {
        &s[self.below(s.len())]
    }
}

fn scale() -> usize {
    std::env::var("VED_FUZZ").ok().and_then(|v| v.parse().ok()).unwrap_or(1).max(1)
}

/// Run `f` on a thread with the stack size of a real main thread; fail on panic or on a hang.
fn guarded<F: FnOnce() + Send + 'static>(what: &str, f: F) {
    let (tx, rx) = channel();
    let h = std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(move || {
            let r = catch_unwind(AssertUnwindSafe(f));
            let _ = tx.send(r.is_ok());
        })
        .unwrap();
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(true) => {
            let _ = h.join();
        }
        Ok(false) => {
            let _ = h.join();
            panic!("{}: panicked (see the message above)", what);
        }
        Err(_) => panic!("{}: still running after 60 s: a hang", what),
    }
}

/// Like `guarded`, but the closure reports what it is doing, so a hang names its seed and the keys.
fn guarded_with_status<F: FnOnce(Arc<Mutex<String>>) + Send + 'static>(what: &str, f: F) {
    let status = Arc::new(Mutex::new(String::new()));
    let st = status.clone();
    let (tx, rx) = channel();
    let h = std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(move || {
            let r = catch_unwind(AssertUnwindSafe(|| f(st)));
            let _ = tx.send(r.is_ok());
        })
        .unwrap();
    // A hang is "the status has not changed for a long time", not "the whole run took long": a
    // big soak (VED_FUZZ=150) is allowed to take minutes as long as it keeps moving.
    let limit = Duration::from_secs(std::env::var("VED_FUZZ_TIMEOUT").ok().and_then(|v| v.parse().ok()).unwrap_or(120));
    let mut last = String::new();
    let mut since = std::time::Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(true) => {
                let _ = h.join();
                return;
            }
            Ok(false) => {
                let _ = h.join();
                panic!("{}: panicked (see the message above)", what);
            }
            Err(_) => {
                let now = status.lock().map(|s| s.clone()).unwrap_or_default();
                if now != last {
                    last = now;
                    since = std::time::Instant::now();
                } else if since.elapsed() > limit {
                    panic!("{}: a hang (no progress for {:?}). Last status: {}", what, limit, last)
                }
            }
        }
    }
}

const CHARS: &[char] = &[
    'a', 'b', 'c', 'x', 'y', 'z', 'A', 'Z', '0', '1', '9', ' ', ' ', ' ', '\t', '_', '-', '.', ',', ';', ':', '(', ')', '[', ']', '{', '}', '<', '>', '"', '\'', '`', '/', '\\', '*', '+', '?', '!',
    '@', '#', '$', '%', '^', '&', '=', '|', '~', '\u{e9}', '\u{fc}', '\u{65e5}', '\u{672c}', '\u{1F600}', '\u{1F468}', '\u{200d}', '\u{301}', '\u{200b}', '\u{feff}', '\u{5d0}', '\u{627}', '\u{7}',
    '\u{1b}', '\u{7f}', '\u{85}', '\r', '\u{0}', '\u{2028}', '\u{fffd}', '\u{10ffff}',
];

const WORDS: &[&str] = &[
    "fn", "let", "if", "else", "return", "struct", "impl", "def", "class", "import", "#include", "<stdio.h>", "foo", "bar", "baz", "main", "x", "y", "TODO", "http://a.b/c", "\"str\"", "'c'", "//", "/*", "*/", "#",
    "{", "}", "(", ")", "=>", "->", "::", "0x1f", "3.14", "-7", "1e9", "nixpkgs", "inputs@{", "...", "}:",
];

fn gen_line(rng: &mut Rng) -> String {
    let mut s = String::new();
    match rng.below(10) {
        0 => {}
        1 => {
            for _ in 0..rng.range(1, 6) {
                s.push(*rng.pick(CHARS));
            }
        }
        2 => {
            // very long line
            for _ in 0..rng.range(500, 6000) {
                s.push(*rng.pick(&CHARS[..20]));
            }
        }
        _ => {
            for _ in 0..rng.range(0, 3) {
                s.push_str(rng.pick(&["", " ", "  ", "    ", "\t"]));
            }
            for _ in 0..rng.range(1, 8) {
                s.push_str(rng.pick(WORDS));
                s.push(if rng.chance(1, 8) { *rng.pick(CHARS) } else { ' ' });
            }
        }
    }
    s
}

fn gen_text(rng: &mut Rng) -> String {
    let n = match rng.below(8) {
        0 => 0,
        1 => 1,
        2 => rng.range(200, 1500),
        _ => rng.range(1, 30),
    };
    let mut t = String::new();
    for i in 0..n {
        if i > 0 {
            t.push('\n');
        }
        t.push_str(&gen_line(rng));
    }
    if rng.chance(3, 4) && n > 0 {
        t.push('\n');
    }
    if rng.chance(1, 10) {
        t = t.replace('\n', "\r\n");
    }
    t
}

// ---- input parser -------------------------------------------------------------------------------

#[test]
fn input_parser_survives_random_bytes() {
    guarded("input parser", || {
        const PARTS: &[&[u8]] = &[
            b"\x1b", b"\x1b[", b"\x1b[1;5C", b"\x1b[H", b"\x1b[200~", b"\x1b[201~", b"\x1bO", b"\x1bOP", b"\x1b[99999999999999999999~", b"\x1b[;;;;H", b"\x1b[?", b"\xc3", b"\xe6\x97", b"\xf0\x9f\x98",
            b"\xff", b"\xc0\x80", b"\x00", b"\r", b"\x7f", b"a", b"\xe6\x97\xa5", b"\x1b\x1b", b"\x1b[1;", b"\x1b[M", b"\x1b[<0;1;1M",
        ];
        for seed in 0..(400 * scale() as u64) {
            let mut rng = Rng::new(seed);
            let mut p = InputParser::new();
            for _ in 0..rng.range(1, 40) {
                let mut chunk = Vec::new();
                for _ in 0..rng.range(1, 6) {
                    if rng.chance(3, 4) {
                        chunk.extend_from_slice(rng.pick(PARTS));
                    } else {
                        chunk.push(rng.below(256) as u8);
                    }
                }
                p.push(&chunk);
                let mut guard = 0;
                while let Some(ev) = p.next_event(rng.chance(1, 3)) {
                    // the raw bytes must be what the key came from, never empty for real keys
                    if !matches!(ev.key, Key::Paste(_)) {
                        assert!(!ev.raw.is_empty(), "seed {}: key {:?} without raw bytes", seed, ev.key);
                    }
                    guard += 1;
                    assert!(guard < 10_000, "seed {}: parser does not terminate", seed);
                }
            }
            while p.next_event(true).is_some() {}
        }
    });
}

// ---- VT emulator --------------------------------------------------------------------------------

fn gen_vt_bytes(rng: &mut Rng) -> Vec<u8> {
    const PIECES: &[&str] = &[
        "\x1b[", "\x1b]", "\x1bP", "\x1b_", "\x1b^", "\x1bX", "\x07", "\x1b\\", "\x1b[?", "\x1b[>", "\x1b[!", "\x1b(", "\x1b)", "\x1b#8", "\x1b7", "\x1b8", "\x1bM", "\x1bD", "\x1bE", "\x1bc",
        "0", "1", "2", "5", "6", "9", "47", "99", "1049", "2004", "1000", "99999999999999999999", "4294967296", "65536", "-1", ";", ":", ";;", "m", "H", "f", "A", "B", "C", "D", "J", "K", "L", "M", "P", "@",
        "X", "S", "T", "r", "s", "u", "h", "l", "n", "c", "t", "q", "g", "d", "G", "e", "a", "`", "b", "Z", "I", "\n", "\r", "\t", "\x08", "\x0b", "\x0c", "\x0e", "\x0f", "\x05", "\x1a", "\x18",
        "hello", "\u{65e5}\u{672c}", "\u{1F600}", "e\u{301}", "\u{200d}", "\u{0}", "\u{7f}", "\u{85}", "\u{9b}", "\u{90}", "\u{9d}",
    ];
    let mut out = Vec::new();
    for _ in 0..rng.range(1, 60) {
        if rng.chance(1, 20) {
            out.push(rng.below(256) as u8);
        } else {
            out.extend_from_slice(rng.pick(PIECES).as_bytes());
        }
    }
    out
}

#[test]
fn vt_survives_random_escape_sequences() {
    guarded("vt", || {
        for seed in 0..(600 * scale() as u64) {
            let mut rng = Rng::new(seed);
            let (mut rows, mut cols) = (rng.range(1, 30), rng.range(1, 90));
            let mut vt = Vt::new(rows, cols);
            for step in 0..rng.range(5, 60) {
                let bytes = gen_vt_bytes(&mut rng);
                vt.feed(&bytes);
                if rng.chance(1, 6) {
                    rows = rng.range(1, 30);
                    cols = rng.range(1, 90);
                    vt.resize(rows, cols);
                }
                assert_eq!((vt.rows, vt.cols), (rows, cols), "seed {} step {}", seed, step);
                assert!(vt.cur.0 < rows && vt.cur.1 < cols, "seed {} step {}: cursor {:?} outside {}x{}", seed, step, vt.cur, rows, cols);
                for r in 0..rows {
                    let v = vt.view_row(r, rng.below(5));
                    assert!(v.len() <= cols.max(1) * 2, "seed {}: row longer than the screen", seed);
                    let _ = vt.row_text(r);
                }
                let _ = vt.text();
                // a terminal must never answer with an echo of attacker-controlled text
                assert!(vt.responses.len() < 4096, "seed {}: unbounded responses", seed);
                vt.responses.clear();
            }
            assert!(vt.scrollback.len() <= 200_000, "seed {}: scrollback is unbounded ({})", seed, vt.scrollback.len());
        }
    });
}

#[test]
fn vt_scrollback_and_titles_are_bounded() {
    guarded("vt bounds", || {
        let mut vt = Vt::new(24, 80);
        for i in 0..300_000 {
            vt.feed(format!("line {}\r\n", i).as_bytes());
        }
        assert!(vt.scrollback.len() <= 200_000, "scrollback grew to {}", vt.scrollback.len());
        let mut title = b"\x1b]0;".to_vec();
        title.extend(std::iter::repeat_n(b'A', 5_000_000));
        title.push(7);
        vt.feed(&title);
        assert!(vt.title.chars().count() < 10_000, "title is {} chars", vt.title.chars().count());
        // an unterminated OSC / DCS must not make us buffer for ever
        let mut open = b"\x1b]52;c;".to_vec();
        open.extend(std::iter::repeat_n(b'B', 20_000_000));
        vt.feed(&open);
        vt.feed(b"\x1b\\ok");
    });
}

// ---- JSON ------------------------------------------------------------------------------------------

#[test]
fn json_parser_survives_garbage_and_deep_nesting() {
    guarded("json", || {
        const PARTS: &[&str] = &[
            "{", "}", "[", "]", ",", ":", "\"", "\"a\"", "\"\\u00e9\"", "\"\\ud83d\\ude00\"", "\"\\ud83d\"", "\"\\udc00\"", "\"\\u12\"", "\"\\x\"", "\\", "null", "true", "false", "nul", "tru", "0", "-0", "1.5e308",
            "1e999", "-1e999", "123456789012345678901234567890", "-", "+", ".", "e", "\u{0}", "\u{1}", "\n", " ", "\t", "\u{65e5}", "[[[[", "{\"a\":", "\"\n\"",
        ];
        for seed in 0..(1500 * scale() as u64) {
            let mut rng = Rng::new(seed);
            let mut s = String::new();
            for _ in 0..rng.range(0, 30) {
                s.push_str(rng.pick(PARTS));
            }
            if let Ok(j) = Json::parse(&s) {
                // whatever parsed must print and parse again
                let again = Json::parse(&j.to_string());
                assert!(again.is_ok(), "seed {}: {:?} printed as {:?} does not parse", seed, s, j.to_string());
            }
        }
        // a hostile server can send a megabyte of '['
        for depth in [100usize, 1_000, 10_000, 100_000, 1_000_000] {
            let open = "[".repeat(depth);
            let _ = Json::parse(&open);
            let full = format!("{}{}", open, "]".repeat(depth));
            let _ = Json::parse(&full);
            let objs = format!("{}1{}", "{\"a\":".repeat(depth), "}".repeat(depth));
            let _ = Json::parse(&objs);
        }
    });
}

// ---- snippets ----------------------------------------------------------------------------------------

#[test]
fn snippet_parser_survives_random_input() {
    guarded("snippet", || {
        const PARTS: &[&str] = &[
            "$", "${", "}", "$0", "$1", "$2", "${1:", "${2:x}", "${1|a,b|}", "${1/x/y/}", "\\", "\\$", "\\}", ":", "|", ",", "/", "\n", "\t", "foo", "(", ")", "$TM_FILENAME", "${TM_FILENAME:x}", "${", "$$", "${0}", "${99999999999999999999}",
            "$99999999999999999999", "\u{65e5}", "\u{1F600}", "${1:${2:${3:deep}}}", "${1:a${1:b}}",
        ];
        for seed in 0..(3000 * scale() as u64) {
            let mut rng = Rng::new(seed);
            let mut s = String::new();
            for _ in 0..rng.range(0, 25) {
                s.push_str(rng.pick(PARTS));
            }
            let snip = ved::snippet::parse_snippet(&s);
            let n = snip.text.chars().count();
            for st in &snip.stops {
                assert!(st.start + st.len <= n, "seed {}: stop outside the text for {:?}", seed, s);
            }
            let adj = ved::snippet::adjust_indent(&s, "  ", "\t");
            let _ = ved::snippet::parse_snippet(&adj);
            // a few levels of nesting
            let deep = format!("{}x{}", "${1:".repeat(rng.range(1, 3000)), "}".repeat(rng.range(0, 3000)));
            let _ = ved::snippet::parse_snippet(&deep);
        }
    });
}

// ---- syntax highlighting -----------------------------------------------------------------------------

#[test]
fn highlighter_survives_random_text_in_every_language() {
    guarded("highlight", || {
        let exts = ["rs", "py", "js", "ts", "c", "cpp", "go", "sh", "json", "toml", "md", "yml", "html", "lua", "java", "nix", "css"];
        for seed in 0..(300 * scale() as u64) {
            let mut rng = Rng::new(seed);
            let path = std::path::PathBuf::from(format!("x.{}", rng.pick(&exts)));
            let Some(lang) = ved::highlight::lang_for(Some(&path)) else { continue };
            let mut state = 0u8;
            for _ in 0..rng.range(1, 30) {
                let line: Vec<char> = gen_line(&mut rng).chars().collect();
                let mut out = Vec::new();
                ved::highlight::highlight_line(lang, &line, &mut state, &mut out);
                assert!(out.len() >= line.len().min(1) || line.is_empty(), "seed {}: no styles for a non-empty line", seed);
            }
        }
    });
}

// ---- buffer ------------------------------------------------------------------------------------------------

#[test]
fn buffer_edits_undo_redo_and_text_roundtrip() {
    guarded("buffer", || {
        for seed in 0..(500 * scale() as u64) {
            let mut rng = Rng::new(seed);
            let text = gen_text(&mut rng);
            let mut b = Buffer::from_text(&text);
            assert!(!b.lines.is_empty());
            // text -> buffer -> text is lossless for UTF-8 text with `\n` or `\r\n` line ends
            if !text.contains('\r') {
                assert_eq!(b.text(), text, "seed {}: round trip", seed);
            }
            let original = b.lines.clone();
            let mut groups = 0;
            for _ in 0..rng.range(1, 40) {
                let last = b.line_count() - 1;
                let pos = |rng: &mut Rng, b: &Buffer| {
                    let r = rng.below(b.line_count() + 2).min(b.line_count() + 1);
                    (r, rng.below(b.line_len(r.min(b.line_count() - 1)) + 3))
                };
                b.begin_group((0, 0));
                for _ in 0..rng.range(1, 4) {
                    let p1 = pos(&mut rng, &b);
                    let p2 = pos(&mut rng, &b);
                    let (a, z) = if p1 <= p2 { (p1, p2) } else { (p2, p1) };
                    match rng.below(6) {
                        0 => {
                            let a = b.clamp(a);
                            b.insert_str(a, &gen_line(&mut rng));
                        }
                        1 => {
                            let a = b.clamp(a);
                            b.insert_str(a, "x\ny\n\nz");
                        }
                        2 => b.delete_range(b.clamp(a), b.clamp(z)),
                        3 => {
                            let r0 = rng.below(last + 1);
                            let r1 = r0 + rng.below(3);
                            b.delete_lines(r0, r1.min(b.line_count() - 1));
                        }
                        4 => {
                            let r = rng.below(b.line_count() + 1);
                            b.insert_lines(r.min(b.line_count()), vec![vec!['q'], vec![]]);
                        }
                        _ => {
                            let r = rng.below(b.line_count());
                            b.set_line(r, gen_line(&mut rng).chars().collect());
                        }
                    }
                    assert!(!b.lines.is_empty(), "seed {}: the buffer became empty", seed);
                }
                b.end_group((0, 0));
                groups += 1;
            }
            let edited = b.lines.clone();
            for _ in 0..groups + 2 {
                b.undo();
            }
            assert_eq!(b.lines, original, "seed {}: undo did not restore the text", seed);
            for _ in 0..groups + 2 {
                b.redo();
            }
            assert_eq!(b.lines, edited, "seed {}: redo did not restore the edits", seed);
        }
    });
}

// ---- the editor -----------------------------------------------------------------------------------------------

fn parse_keys(s: &str) -> Vec<Key> {
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
                    "Tab" => Some(Key::Tab),
                    "S-Tab" => Some(Key::BackTab),
                    "Left" => Some(Key::Left),
                    "Right" => Some(Key::Right),
                    "Up" => Some(Key::Up),
                    "Down" => Some(Key::Down),
                    "Home" => Some(Key::Home),
                    "End" => Some(Key::End),
                    "Del" => Some(Key::Delete),
                    "lt" => Some(Key::Char('<')),
                    n if n.starts_with("C-") && n.chars().count() == 3 => Some(Key::Ctrl(n.chars().nth(2).unwrap())),
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

/// Normal / insert / visual commands and ex commands that do not touch the file system or start
/// processes.
const TOKENS: &[&str] = &[
    "h", "j", "k", "l", "w", "b", "e", "W", "B", "E", "ge", "0", "^", "$", "gg", "G", "{", "}", "(", ")", "%", "H", "M", "L", "fx", "Fx", "tx", "Tx", ";", ",", "x", "X", "dd", "D", "cc", "C", "s", "S", "ra", "J", "gJ",
    "u", "<C-r>", ".", "p", "P", "yy", "Y", "yw", "y$", "dw", "d$", "dj", "dk", "dG", "dgg", "daw", "diw", "ci(", "ci\"", "da{", "dap", "dip", "cw", "c$", ">>", "<<", ">j", "=", "==", "~", "g~~", "gUU", "guu", "gUiw", "i", "a",
    "I", "A", "o", "O", "v", "V", "<C-v>", "gv", "o", "<Esc>", "<Esc>", "<CR>", "<BS>", "<Tab>", "<S-Tab>", "<Left>", "<Right>", "<Up>", "<Down>", "<Home>", "<End>", "<Del>", "<C-w>", "<C-u>", "<C-h>", "<C-d>", "<C-t>",
    "<C-n>", "<C-p>", "<C-o>", "<C-i>", "<C-a>", "<C-x>", "<C-e>", "<C-y>", "<C-f>", "<C-b>", "<C-g>", "<C-l>", "<C-c>", "<C-[>", "/a<CR>", "/x<CR>", "?b<CR>", "n", "N", "*", "#", "qa", "q", "@a", "@@", "ma", "mb", "'a",
    "`a", "''", "``", "zz", "zt", "zb", "zo", "zc", "gi", "gI", "g;", "gp", "gP", "xp", "ddp", "yyP", "K", "gd", "gr", "]d", "[d", "gx", "ga", "g8", "&", "g&", "\"ayy", "\"ap", "\"_dd", "\"+y", "\"+p", "99p", "5x",
    "3dd", "2dw", "100000p", "99999999999999999999x", "999999999dd", "100000ix<Esc>", "0D", "ggVGd", "ggVG>", "ggVGJ", ":%d<CR>", ":%s/a/b/g<CR>", ":%s/./&&/g<CR>", ":%s/$/x/<CR>", ":%s/^/y/<CR>",
    ":s/\\(a\\)/\\1\\1/<CR>", ":s/x*//g<CR>", ":s/\\n//<CR>", ":%s/\\t/  /g<CR>", ":%s/ *$//<CR>", ":%s/a/\\r/g<CR>", ":%s/a/\\n/g<CR>", ":g/a/d<CR>", ":v/a/d<CR>", ":g/a/normal Ax<CR>", ":g/./normal dd<CR>",
    ":%normal Ix<CR>", ":%normal @a<CR>", ":%norm dwP<CR>", ":sort<CR>", ":sort!<CR>", ":sort n<CR>", ":sort u<CR>", ":%j<CR>", ":m0<CR>", ":m$<CR>", ":t.<CR>", ":t0<CR>", ":1,3d<CR>", ":2,4>", ":$<CR>", ":1<CR>",
    ":9999<CR>", ":-1<CR>", ":+1<CR>", ":0<CR>", ":'<,'>d<CR>", ":.,$y<CR>", ":put<CR>", ":$pu<CR>", ":0pu<CR>", ":2,1d<CR>", ":set nu<CR>", ":set rnu<CR>", ":set nonu<CR>", ":set sw=0<CR>", ":set sw=8<CR>", ":set ts=0<CR>",
    ":set ts=16<CR>", ":set sw=99999999999999999999<CR>", ":set et<CR>", ":set noet<CR>", ":set noai<CR>", ":set ai<CR>", ":set noap<CR>", ":set theme=terminal<CR>", ":set theme=dark<CR>", ":set ic scs<CR>", ":noh<CR>",
    ":reg<CR>", ":marks<CR>", ":help<CR>", ":lsp<CR>", ":ls<CR>", ":pwd<CR>", ":undo<CR>", ":redo<CR>", ":enew<CR>", ":bn<CR>", ":bp<CR>", ":b1<CR>", ":b9<CR>", ":bd!<CR>", ":diag<CR>", ":refs<CR>", ":k a<CR>", ":dump<CR>",
    "<C-w>h", "<C-w>l", "<C-w>w", ":tree<CR>", ":sidebar<CR>", ":explore<CR>", ":termhide<CR>", ":set nosidebar<CR>", ":set sidebar<CR>", ":set sbw=1<CR>", ":set sbw=5<CR>", ":set sbw=40<CR>", ":set sbw=99999999999999999999<CR>",
    ":set mouse<CR>", ":set nomouse<CR>", ":files<CR>", "<Esc><Esc>", "gg<CR>", "jjj<CR>", "l", "h", "<Space>", "r", "R",
    // the panel's file operations: `a` file, `f` folder, `d` file / `dd` folder (asks y/N); names that try to
    // get out of the project, to touch `.git`, to go through links, or to be far too long
    "<C-w>ha", "<C-w>hf", "<C-w>hd", "<C-w>hdd", "<C-w>hjjd", "<C-w>hjjdd", "y", "Y", "n", "N", "a", "f", "d", "dd", "<C-w>hjjj<CR>", "<C-w>hggjjjjjjjj", "<C-w>hjdy", "<C-w>hjjdy", "<C-w>hjjjdy", "<C-w>hjjjjdy", "<C-w>hjddy", "<C-w>hjjddy", "<C-w>hjjjddy", "<C-w>hjjjjjddy", "dy", "ddy", "<C-w>hjjjjjjjjdy",
    "new.txt<CR>", "src/new.rs<CR>", "src/deep/er/more/x.txt<CR>", "a/b/c/d/e.txt<CR>", "newdir<CR>", "newdir/sub/<CR>", "README.md<CR>", "src<CR>",
    "../ved-escape-marker<CR>", "../../ved-escape-marker2<CR>", "/tmp/ved-fuzz-abs-marker<CR>", "src/../../ved-escape-marker3<CR>",
    ".git<CR>", ".git/config<CR>", "src/.git/x<CR>", "..<CR>", ".<CR>", "./<CR>", "//<CR>", "a//b<CR>", "loop/x.txt<CR>", "loop/loop/x<CR>", "dangling/x<CR>", "pipe/x<CR>", "pipe<CR>",
    "x\u{1b}y<CR>", "name with spaces.txt<CR>", " <CR>", "<CR>", "<BS><BS><BS><BS>", "<C-u>", "<C-w>",
    ":set noconfirmdelete<CR>", ":set confirmdelete<CR>", ":set cfd<CR>", ":set noindentguides<CR>", ":set indentguides<CR>", ":set noig<CR>", ":set ig<CR>", ":set indentchar=|<CR>",
    ":set indentchar=\u{65e5}<CR>", ":set indentchar=ab<CR>", ":set indentchar=<CR>", ":set indentchar=\t<CR>", ":set indentchar=\u{301}<CR>", ":cd src<CR>", ":cd src/deep<CR>", ":cd<CR>",
    ":99999999999999999999<CR>", ":.+99999999999999999999d<CR>", ":.-99999999999999999999<CR>", ":1,99999999999999999999d<CR>", ":set sw=-1<CR>", ":set ts=-1<CR>", ":b99999999999999999999<CR>", ":bogus<CR>", ":<CR>", ":<BS><BS><BS>", ":'a,'bd<CR>", ":%y<CR>", ":%!", "<C-r>a", "<C-r><C-w>",
];

/// A mouse event anywhere: inside the screen, on its edges, far outside it, with every kind of button.
fn gen_mouse(rng: &mut Rng) -> Key {
    const BUTTONS: &[u16] = &[0, 0, 0, 1, 2, 3, 4, 8, 16, 32, 35, 64, 64, 65, 65, 66, 67, 68, 96, 127, 128, 1000, u16::MAX];
    let coord = |rng: &mut Rng| -> u16 {
        match rng.below(8) {
            0 => 0,
            1 => 1,
            2 => u16::MAX,
            3 => rng.range(0, 3000) as u16,
            _ => rng.range(0, 60) as u16,
        }
    };
    let button = *rng.pick(BUTTONS);
    let row = coord(rng);
    let col = coord(rng);
    Key::Mouse(Mouse { button, row, col, release: rng.chance(1, 3) })
}

/// A press, some movements with the button held and a release (or not: the release can get lost),
/// starting on the panel's edge, on a tab or anywhere.
fn gen_drag(rng: &mut Rng) -> Vec<Key> {
    let (row, col) = match rng.below(4) {
        0 => (rng.range(0, 30) as u16, [15u16, 16, 29, 30, 31, 44, 59, 79][rng.below(8)]),
        1 => (0, rng.range(0, 140) as u16),
        _ => (rng.range(0, 45) as u16, rng.range(0, 140) as u16),
    };
    let button = if rng.chance(4, 5) { 0 } else { rng.range(0, 3) as u16 };
    let mut out = vec![Key::Mouse(Mouse { button, row, col, release: false })];
    for _ in 0..rng.below(6) {
        let b = if rng.chance(5, 6) { 32 + button } else { *rng.pick(&[35u16, 64, 65, 0]) };
        let c = if rng.chance(1, 8) { u16::MAX } else { rng.range(0, 150) as u16 };
        out.push(Key::Mouse(Mouse { button: b, row: rng.range(0, 45) as u16, col: c, release: false }));
    }
    if rng.chance(4, 5) {
        out.push(Key::Mouse(Mouse { button, row, col: rng.range(0, 150) as u16, release: true }));
    }
    out
}

fn gen_keys(rng: &mut Rng) -> Vec<Key> {
    let mut out = Vec::new();
    for _ in 0..rng.range(1, 6) {
        match rng.below(15) {
            13 | 14 => out.extend(gen_drag(rng)),
            10..=12 => out.push(gen_mouse(rng)),
            0..=5 => out.extend(parse_keys(rng.pick(TOKENS))),
            6 => out.push(Key::Char(*rng.pick(CHARS))),
            7 if rng.chance(1, 6) => {
                // a name that is far too long, or a path that is far too deep, typed or pasted
                let piece = *rng.pick(&["x", "ab/", "../", "\u{65e5}", "x/", ".", "\u{301}"]);
                let n = *rng.pick(&[60usize, 300, 1100, 5000]);
                let s: String = piece.repeat(n);
                if rng.chance(1, 2) {
                    out.push(Key::Paste(s));
                } else {
                    // typed into a name prompt (a flood of `.` in normal mode would repeat an earlier,
                    // possibly heavy, command a thousand times: slow by design, and not what is tested)
                    out.extend(parse_keys(if rng.chance(1, 2) { "<C-w>ha" } else { "<C-w>hf" }));
                    out.extend(s.chars().take(1200).map(Key::Char));
                }
                out.push(Key::Enter);
            }
            7 => {
                let n = rng.range(0, 12);
                let s: String = (0..n).map(|_| *rng.pick(CHARS)).collect();
                out.push(Key::Paste(s));
            }
            8 => out.push(
                rng.pick(&[
                    Key::Delete,
                    Key::Insert,
                    Key::PageUp,
                    Key::PageDown,
                    Key::Home,
                    Key::End,
                    Key::F(1),
                    Key::F(12),
                    Key::Unknown,
                    Key::Alt('x'),
                    Key::Alt('s'),
                    Key::Alt('q'),
                    Key::Alt('w'),
                    Key::Alt('e'),
                    Key::Alt('b'),
                    Key::Ctrl(' '),
                    Key::Ctrl('\\'),
                    Key::Ctrl(']'),
                    Key::Ctrl('^'),
                    Key::Ctrl('_'),
                ])
                .clone(),
            ),
            _ => out.push(Key::Ctrl((b'a' + rng.below(26) as u8) as char)),
        }
    }
    out
}

fn check_editor(ed: &Editor, seed: u64, trace: &[String]) {
    let bad = |what: String| -> ! {
        let tail: Vec<&String> = trace.iter().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect();
        panic!("seed {}: {}\nlast keys: {:?}", seed, what, tail)
    };
    let sw = ed.sidebar_cols();
    if sw != 0 && !(16..=80).contains(&sw) || sw != 0 && ed.cols < sw + 30 {
        bad(format!("the panel is {} columns wide on a {}-column terminal", sw, ed.cols));
    }
    if let Some(p) = &ed.prompt {
        if p.input.chars().count() > 1000 {
            bad(format!("a prompt holding {} characters", p.input.chars().count()));
        }
    }
    if ed.confirm_close.is_some_and(|i| i >= ed.docs.len()) {
        bad(format!("a close confirmation for buffer {:?} of {}", ed.confirm_close, ed.docs.len()));
    }
    if ed.alt.is_some_and(|a| a >= ed.docs.len()) {
        bad(format!("alternate buffer {:?} of {}", ed.alt, ed.docs.len()));
    }
    if ed.jumps.iter().any(|(d, _)| *d >= ed.docs.len()) {
        bad("a jump to a buffer that is gone".into());
    }
    if !ed.tree.rows.is_empty() && ed.tree.sel >= ed.tree.rows.len() {
        bad(format!("panel selection {} of {} rows", ed.tree.sel, ed.tree.rows.len()));
    }
    if ed.tree.top > ed.tree.rows.len() {
        bad(format!("panel scrolled to {} of {} rows", ed.tree.top, ed.tree.rows.len()));
    }
    if ed.tree.rows.len() > 60_000 {
        bad(format!("panel has {} rows", ed.tree.rows.len()));
    }
    if ed.docs.is_empty() {
        bad("no documents left".into());
    }
    if ed.cur >= ed.docs.len() {
        bad(format!("current document {} of {}", ed.cur, ed.docs.len()));
    }
    for (i, d) in ed.docs.iter().enumerate() {
        if d.buf.lines.is_empty() {
            bad(format!("document {} has no lines", i));
        }
        let (r, c) = d.cursor;
        if r >= d.buf.lines.len() {
            bad(format!("document {}: cursor row {} of {}", i, r, d.buf.lines.len()));
        }
        if c > d.buf.lines[r].len() {
            bad(format!("document {}: cursor column {} past the end of a {}-char line", i, c, d.buf.lines[r].len()));
        }
    }
}

fn check_frame(f: &Frame, seed: u64, trace: &[String]) {
    for r in 0..f.rows {
        for c in 0..f.cols {
            let ch = f.get(r, c).ch;
            if ch.is_control() && ch != '\0' {
                let tail: Vec<&String> = trace.iter().rev().take(40).collect::<Vec<_>>().into_iter().rev().collect();
                panic!("seed {}: control character {:?} at {},{} would reach the terminal\nlast keys: {:?}", seed, ch, r, c, tail);
            }
        }
    }
}

/// Random keys include `:w name`, `:e name` and `:%!` followed by random text, which is a real shell
/// command (a stray `>` creates a file). Run them in a throw-away directory, with nothing on PATH and
/// a private HOME, so that no random command can touch the sources or anything else.
fn contain_the_editor() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join("ved-fuzz-sandbox");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("empty")).unwrap();
        std::fs::create_dir_all(dir.join("home")).unwrap();
        std::env::set_current_dir(&dir).unwrap();
        std::env::set_var("PATH", dir.join("empty"));
        std::env::set_var("HOME", dir.join("home"));
        std::env::set_var("XDG_STATE_HOME", dir.join("state"));
        std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
    });
    restock();
}

fn sandbox() -> std::path::PathBuf {
    std::env::temp_dir().join("ved-fuzz-sandbox")
}

/// The panel's file operations make and delete things, so every case starts from the same project: this
/// puts back what an earlier case deleted and removes what it made (only ever inside the sandbox).
fn restock() {
    let dir = sandbox();
    assert!(dir.ends_with("ved-fuzz-sandbox"));
    let keep = ["empty", "home", "src", "target", "README.md", "w\u{eb}ird \u{65e5}\u{672c}.txt", "with\u{1b}escape", "loop", "dangling", "pipe", "state", "config"];
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name();
            if keep.iter().any(|k| std::ffi::OsStr::new(k) == name) {
                continue;
            }
            match std::fs::symlink_metadata(e.path()) {
                Ok(m) if m.is_dir() => {
                    let _ = std::fs::remove_dir_all(e.path());
                }
                Ok(_) => {
                    let _ = std::fs::remove_file(e.path());
                }
                Err(_) => {}
            }
        }
    }
    // something for the project panel to show: nested directories, links (one back to the root,
    // one dangling), a named pipe, odd names
    let _ = std::fs::create_dir_all(dir.join("src/deep/er"));
    let _ = std::fs::create_dir_all(dir.join("target/debug"));
    let _ = std::fs::create_dir_all(dir.join("home"));
    let _ = std::fs::create_dir_all(dir.join("empty"));
    let _ = std::fs::write(dir.join("src/a.rs"), "fn a() {}\n    if x {\n\n        y\n    }\n");
    let _ = std::fs::write(dir.join("src/deep/b.txt"), "b\n");
    let _ = std::fs::write(dir.join("src/deep/er/c.md"), "# c\n");
    let _ = std::fs::write(dir.join("README.md"), "# readme\n");
    let _ = std::fs::write(dir.join("w\u{eb}ird \u{65e5}\u{672c}.txt"), "x\n");
    let _ = std::fs::write(dir.join("with\u{1b}escape"), "x\n");
    let _ = std::os::unix::fs::symlink(".", dir.join("loop"));
    let _ = std::os::unix::fs::symlink("nowhere", dir.join("dangling"));
    extern "C" {
        fn mkfifo(path: *const std::os::raw::c_char, mode: u32) -> i32;
    }
    let fifo = std::ffi::CString::new(dir.join("pipe").to_string_lossy().as_bytes()).unwrap();
    unsafe {
        mkfifo(fifo.as_ptr(), 0o600);
    }
    // every case starts at the project's top (an earlier `:cd` moved the whole process)
    let _ = std::env::set_current_dir(&dir);
}

/// Nothing a random name could do may leave a marker outside the project: a path with `..`, an absolute
/// path, `~`. Looks in the sandbox (whatever the project root was after a `:cd`) and in its parent.
fn no_escapes() {
    fn walk(dir: &std::path::Path, depth: usize, found: &mut Vec<std::path::PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("ved-escape-marker") {
                found.push(e.path());
            }
            let is_real_dir = std::fs::symlink_metadata(e.path()).map(|m| m.is_dir()).unwrap_or(false);
            // `abc../ved-escape-marker` is a folder called `abc..` with a file in it: a legal name that
            // the random typing made out of earlier characters, not an escape
            if is_real_dir && depth < 8 && !name.ends_with("..") {
                walk(&e.path(), depth + 1, found);
            }
        }
    }
    let mut found = Vec::new();
    walk(&sandbox(), 0, &mut found);
    for d in [std::env::temp_dir(), std::path::PathBuf::from("/tmp"), std::path::PathBuf::from("/")] {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with("ved-escape-marker") {
                    found.push(e.path());
                }
            }
        }
    }
    // an absolute name is the one place a prefix typed before it cannot matter: it exists only if it escaped
    for abs in [std::env::temp_dir().join("ved-fuzz-abs-marker"), std::path::PathBuf::from("/tmp/ved-fuzz-abs-marker")] {
        if std::fs::symlink_metadata(&abs).is_ok() {
            found.push(abs);
        }
    }
    assert!(found.is_empty(), "a typed name escaped the project: {:?}", found);
}

/// How much of the panel's file work the random keys really reach (printed with `--nocapture`).
static SEEN_PROMPTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static SEEN_CREATED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static SEEN_DELETED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static SEEN_REFUSED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn run_editor_case(seed: u64, steps: usize, status: &Mutex<String>) {
    contain_the_editor();
    // a small cap, so that a runaway `100000p` is refused long before it could use the machine's memory
    ved::buffer::set_max_chars(2 << 20);
    let mut rng = Rng::new(seed);
    let (mut rows, mut cols) = match rng.below(6) {
        0 => (rng.range(1, 4), rng.range(1, 12)),
        _ => (rng.range(5, 45), rng.range(10, 140)),
    };
    let mut opts = Options::default();
    opts.lsp = false;
    opts.osc52 = rng.chance(1, 2);
    opts.number = rng.chance(2, 3);
    opts.relativenumber = rng.chance(1, 4);
    opts.detectindent = rng.chance(1, 3);
    opts.shiftwidth = rng.range(1, 8);
    opts.tabstop = rng.range(1, 8);
    opts.expandtab = rng.chance(2, 3);
    opts.sidebar = rng.chance(3, 4);
    opts.sidebar_width = rng.range(1, 80);
    opts.mouse = rng.chance(3, 4);
    // a terminal pane runs this: `cat` echoes what it is sent and can do nothing else
    opts.shell = "/bin/cat".into();
    let mut ed = Editor::new(opts, rows, cols);
    ed.docs[0].buf = Buffer::from_text(&gen_text(&mut rng));
    let mut frame = Frame::new(rows, cols);
    let mut rend = Renderer::new();
    let mut trace: Vec<String> = Vec::new();
    for _ in 0..steps {
        for k in gen_keys(&mut rng) {
            trace.push(format!("{:?}", k));
            if let Ok(mut st) = status.lock() {
                let tail: Vec<&String> = trace.iter().rev().take(25).collect::<Vec<_>>().into_iter().rev().collect();
                // the step number is part of the status: a flood of the same key would otherwise look like a hang
                *st = format!("seed {} ({}x{}), step {}, keys: {:?}", seed, rows, cols, trace.len(), tail);
                // the process can be killed (out of memory): leave the last status on disk too
                if let Ok(path) = std::env::var("VED_FUZZ_TRACE") {
                    let _ = std::fs::write(path, st.as_bytes());
                }
            }
            let had_prompt = ed.prompt.is_some();
            ed.handle_event(Event { key: k, raw: Vec::new() });
            ed.quit = false;
            {
                use std::sync::atomic::Ordering::Relaxed;
                if !had_prompt && ed.prompt.is_some() {
                    SEEN_PROMPTS.fetch_add(1, Relaxed);
                }
                if had_prompt && ed.prompt.is_none() {
                    let m = ed.msg.as_str();
                    if m.starts_with("created") {
                        SEEN_CREATED.fetch_add(1, Relaxed);
                    } else if m.starts_with("deleted") {
                        SEEN_DELETED.fetch_add(1, Relaxed);
                    } else if !m.is_empty() {
                        SEEN_REFUSED.fetch_add(1, Relaxed);
                    }
                }
            }
            check_editor(&ed, seed, &trace);
        }
        if rng.chance(1, 4) {
            rows = rng.range(1, 45);
            cols = rng.range(1, 140);
            ed.rows = rows;
            ed.cols = cols;
            frame = Frame::new(rows, cols);
            rend.invalidate();
            trace.push(format!("resize {}x{}", rows, cols));
        }
        ed.draw(&mut frame);
        check_frame(&frame, seed, &trace);
        let out = rend.render(&frame);
        // whatever we send to the terminal must be our own sequences: no raw control characters
        // besides ESC and the line breaks the renderer itself writes
        if let Some(bad) = out.chars().find(|c| c.is_control() && !matches!(c, '\x1b' | '\r' | '\n')) {
            panic!("seed {}: the renderer wrote the raw control character {:?}", seed, bad);
        }
        let _ = std::mem::take(&mut ed.out_seq);
        ed.tick();
        check_editor(&ed, seed, &trace);
    }
}

#[test]
fn editor_survives_random_keys() {
    let per_thread = 4;
    for t in 0..per_thread {
        guarded_with_status("editor", move |status| {
            let n = 120 * scale() as u64 / per_thread as u64;
            for i in 0..n {
                run_editor_case(t as u64 * 1_000_000 + i, 60, &status);
                no_escapes();
            }
        });
    }
    use std::sync::atomic::Ordering::Relaxed;
    eprintln!(
        "panel prompts opened {}, created {}, deleted {}, refused or cancelled {}",
        SEEN_PROMPTS.load(Relaxed),
        SEEN_CREATED.load(Relaxed),
        SEEN_DELETED.load(Relaxed),
        SEEN_REFUSED.load(Relaxed)
    );
}

/// `VED_FUZZ_SEED=2000344 cargo test --test fuzz replay_one_seed` re-runs one editor case.
#[test]
fn replay_one_seed() {
    let Some(seed) = std::env::var("VED_FUZZ_SEED").ok().and_then(|v| v.parse::<u64>().ok()) else { return };
    guarded_with_status("editor (one seed)", move |status| run_editor_case(seed, 60, &status));
}
