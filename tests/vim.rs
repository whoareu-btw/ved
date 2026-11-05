#![allow(clippy::field_reassign_with_default)]
//! Headless tests: feed key sequences to the editor and compare the resulting buffer.

use ved::config::Options;
use ved::editor::{Editor, Mode};
use ved::input::Key;

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
                    "lt" => Some(Key::Char('<')),
                    n if n.starts_with("C-") && n.len() == 3 => Some(Key::Ctrl(n.chars().nth(2).unwrap())),
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

fn editor(text: &str) -> Editor {
    let mut opts = Options::default();
    opts.osc52 = false;
    opts.lsp = false;
    let mut ed = Editor::new(opts, 24, 80);
    ed.docs[0].buf = ved::buffer::Buffer::from_text(text);
    ed
}

fn feed(ed: &mut Editor, keys: &str) {
    for k in parse_keys(keys) {
        ed.handle_key(k);
    }
}

/// Run keys on `text`; return (buffer text, cursor).
fn run(text: &str, keys: &str) -> (String, (usize, usize)) {
    let mut ed = editor(text);
    feed(&mut ed, keys);
    (ed.buf().text(), ed.cursor())
}

fn t(text: &str, keys: &str) -> String {
    run(text, keys).0
}

#[test]
fn delete_word_and_line() {
    assert_eq!(t("foo bar baz\n", "dw"), "bar baz\n");
    assert_eq!(t("foo bar baz\n", "wdw"), "foo baz\n");
    assert_eq!(t("foo bar baz\n", "d2w"), "baz\n");
    assert_eq!(t("foo bar baz\n", "2dw"), "baz\n");
    assert_eq!(t("a\nb\nc\n", "dd"), "b\nc\n");
    assert_eq!(t("a\nb\nc\n", "2dd"), "c\n");
    assert_eq!(t("a\nb\nc\n", "jdd"), "a\nc\n");
    assert_eq!(t("a\nb\nc\n", "Gdd"), "a\nb\n");
    assert_eq!(t("a\nb\nc\n", "dj"), "c\n");
    assert_eq!(t("a\nb\nc\n", "jdk"), "c\n");
    assert_eq!(t("a\nb\nc\n", "dG"), "");
    assert_eq!(t("a\nb\nc\n", "jdgg"), "c\n");
}

#[test]
fn dw_at_end_of_line_keeps_the_newline() {
    assert_eq!(t("foo\n  bar\n", "dw"), "\n  bar\n");
    assert_eq!(t("a b\nc d\n", "wd2w"), "a d\n"); // d2w from 'b' reaches 'd'; the newline is deleted too
}

#[test]
fn change_word() {
    assert_eq!(t("foo bar\n", "cwxyz<Esc>"), "xyz bar\n");
    assert_eq!(t("foo bar\n", "wcwxyz<Esc>"), "foo xyz\n");
    assert_eq!(t("foo   bar\n", "lllcwX<Esc>"), "fooXbar\n");
    assert_eq!(t("foo bar\n", "cc<Esc>"), "\n");
    assert_eq!(t("foo bar\n", "ccnew<Esc>"), "new\n");
    assert_eq!(t("foo bar\n", "wCz<Esc>"), "foo z\n");
    assert_eq!(t("foo bar\n", "sX<Esc>"), "Xoo bar\n");
}

#[test]
fn x_and_replace() {
    assert_eq!(t("hello\n", "x"), "ello\n");
    assert_eq!(t("hello\n", "3x"), "lo\n");
    assert_eq!(t("hello\n", "$x"), "hell\n");
    assert_eq!(t("hello\n", "Xx"), "ello\n");
    assert_eq!(t("hello\n", "$X"), "helo\n");
    assert_eq!(t("hello\n", "rj"), "jello\n");
    assert_eq!(t("hello\n", "3rx"), "xxxlo\n");
    assert_eq!(t("hello\n", "9rx"), "hello\n"); // not enough chars: no change
    assert_eq!(t("hello\n", "~"), "Hello\n");
    assert_eq!(t("hello\n", "3~"), "HELlo\n");
}

#[test]
fn insert_variants() {
    assert_eq!(t("abc\n", "iX<Esc>"), "Xabc\n");
    assert_eq!(t("abc\n", "aX<Esc>"), "aXbc\n");
    assert_eq!(t("abc\n", "AX<Esc>"), "abcX\n");
    assert_eq!(t("  abc\n", "IX<Esc>"), "  Xabc\n");
    assert_eq!(t("  abc\n", "gIX<Esc>"), "X  abc\n");
    assert_eq!(t("a\nb\n", "ox<Esc>"), "a\nx\nb\n");
    assert_eq!(t("a\nb\n", "Ox<Esc>"), "x\na\nb\n");
    assert_eq!(t("abc\n", "3ix<Esc>"), "xxxabc\n");
    assert_eq!(t("a\n", "3ob<Esc>"), "a\nb\nb\nb\n");
    assert_eq!(t("abc\n", "ix<CR>y<Esc>"), "x\nyabc\n");
}

#[test]
fn autoindent() {
    assert_eq!(t("    a\n", "ob<Esc>"), "    a\n    b\n");
    assert_eq!(t("    a\n", "A<CR>b<Esc>"), "    a\n    b\n");
    assert_eq!(t("fn f() {\n", "A<CR>x<Esc>"), "fn f() {\n  x\n");
    assert_eq!(t("{}\n", "a<CR><Esc>"), "{\n\n}\n"); // untouched auto-indent is removed again
}

#[test]
fn backspace_and_ctrl_w_in_insert() {
    assert_eq!(t("abc\n", "A<BS><BS>X<Esc>"), "aX\n");
    assert_eq!(t("a\nb\n", "ji<BS><Esc>"), "ab\n");
    assert_eq!(t("foo bar\n", "A<C-w>X<Esc>"), "foo X\n");
    assert_eq!(t("foo bar\n", "A<C-u>X<Esc>"), "X\n");
}

#[test]
fn yank_and_paste() {
    assert_eq!(t("a\nb\n", "yyp"), "a\na\nb\n");
    assert_eq!(t("a\nb\n", "yyP"), "a\na\nb\n");
    assert_eq!(t("a\nb\n", "yyjp"), "a\nb\na\n");
    assert_eq!(t("a\nb\n", "yy3p"), "a\na\na\na\nb\n");
    assert_eq!(t("foo bar\n", "ywP"), "foo foo bar\n");
    assert_eq!(t("foo bar\n", "yiwwp"), "foo bfooar\n");
    assert_eq!(t("abc\n", "xp"), "bac\n");
    assert_eq!(t("a\nb\nc\n", "ddp"), "b\na\nc\n");
    assert_eq!(t("a\nb\nc\n", "yyjddP"), "a\nb\nc\n");
    assert_eq!(t("abc\n", "yl3p"), "aaaabc\n");
}

#[test]
fn registers() {
    assert_eq!(t("a b\n", "\"ayiww\"byiw$\"ap\"bp"), "a bab\n");
    assert_eq!(t("one\ntwo\n", "\"ayyj\"Ayy\"ap"), "one\ntwo\none\ntwo\n");
    assert_eq!(t("abc\n", "\"_xp"), "bc\n".to_string() + "");
    assert_eq!(t("a\nb\n", "yyjdd\"0p"), "a\na\n");
    assert_eq!(t("a\nb\nc\n", "dddd\"2p"), "c\na\n");
}

#[test]
fn undo_redo() {
    assert_eq!(t("abc\n", "xu"), "abc\n");
    assert_eq!(t("abc\n", "xxu"), "bc\n");
    assert_eq!(t("abc\n", "xxuu"), "abc\n");
    assert_eq!(t("abc\n", "xxuu<C-r>"), "bc\n");
    assert_eq!(t("abc\n", "ihello <Esc>u"), "abc\n");
    assert_eq!(t("a\nb\nc\n", "ddjddu"), "b\nc\n");
    assert_eq!(t("a\nb\n", "ox<CR>y<Esc>u"), "a\nb\n");
    assert_eq!(t("abc\n", "3x2u"), "abc\n");
}

#[test]
fn dot_repeat() {
    assert_eq!(t("a b c d\n", "dw."), "c d\n");
    assert_eq!(t("a b c d\n", "dw2."), "d\n"); // a count on . replaces the original
    assert_eq!(t("abc abc\n", "cwX<Esc>w."), "X X\n");
    assert_eq!(t("a\nb\nc\n", "dd."), "c\n");
    assert_eq!(t("a\nb\n", "Ax<Esc>j."), "ax\nbx\n");
    assert_eq!(t("a\nb\n", "ox<Esc>."), "a\nx\nx\nb\n");
    assert_eq!(t("abcdef\n", "x..."), "ef\n"); // x plus three repeats
    assert_eq!(t("a a a\n", "rbw."), "b b a\n");
}

#[test]
fn text_objects() {
    assert_eq!(t("foo bar baz\n", "wdiw"), "foo  baz\n");
    assert_eq!(t("foo bar baz\n", "wdaw"), "foo baz\n");
    assert_eq!(t("foo bar baz\n", "wciwX<Esc>"), "foo X baz\n");
    assert_eq!(t("f(a, b)\n", "fadi("), "f()\n");
    assert_eq!(t("f(a, b)\n", "fada("), "f\n");
    assert_eq!(t("say \"hi there\" now\n", "fhdi\""), "say \"\" now\n");
    assert_eq!(t("say \"hi there\" now\n", "fhda\""), "say now\n");
    assert_eq!(t("x [1, 2] y\n", "f1di["), "x [] y\n");
    assert_eq!(t("if x {\n    a;\n    b;\n}\n", "jdi{"), "if x {\n}\n");
    assert_eq!(t("a\nb\n\nc\n", "dip"), "\nc\n");
    assert_eq!(t("a\nb\n\nc\n", "dap"), "c\n");
    assert_eq!(t("foo bar\n", "yiwP"), "foofoo bar\n");
    assert_eq!(t("a (b (c) d) e\n", "fcdi("), "a (b () d) e\n");
    assert_eq!(t("a (b (c) d) e\n", "fcd2i("), "a () e\n");
}

#[test]
fn find_motions() {
    assert_eq!(t("a,b,c\n", "df,"), "b,c\n");
    assert_eq!(t("a,b,c\n", "dt,"), ",b,c\n");
    assert_eq!(t("a,b,c\n", "f,;x"), "a,bc\n");
    assert_eq!(t("a,b,c\n", "$F,x"), "a,bc\n");
    assert_eq!(t("a,b,c\n", "2f,x"), "a,bc\n");
    assert_eq!(t("a,b,c\n", "f,;,x"), "ab,c\n");
    assert_eq!(t("a,b,c\n", "t,;x"), "a,,c\n"); // `;` after `t` skips the comma we are already next to
}

#[test]
fn word_motions_and_counts() {
    assert_eq!(t("one two three\n", "wx"), "one wo three\n");
    assert_eq!(t("one two three\n", "2wx"), "one two hree\n");
    assert_eq!(t("one two three\n", "ex"), "on two three\n");
    assert_eq!(t("one two three\n", "$bx"), "one two hree\n");
    assert_eq!(t("one.two three\n", "Wx"), "one.two hree\n");
    assert_eq!(t("one.two three\n", "wx"), "onetwo three\n");
    assert_eq!(t("a-b\n", "dW"), "\n"); // an empty line is left, not an empty file
}

#[test]
fn line_motions() {
    assert_eq!(t("  abc def\n", "$x"), "  abc de\n");
    assert_eq!(t("  abc def\n", "^x"), "  bc def\n");
    assert_eq!(t("  abc def\n", "$0x"), " abc def\n");
    assert_eq!(t("  abc def\n", "d0"), "  abc def\n".to_string()); // at col 0 already
    assert_eq!(t("  abc def\n", "wd0"), "abc def\n"); // deletes the indent
    assert_eq!(t("abc def\n", "wD"), "abc \n");
    assert_eq!(t("abc def\n", "wd$"), "abc \n");
    assert_eq!(t("a\nb\nc\n", "2Jx"), "ab\nc\n"); // cursor sits on the inserted space
}

#[test]
fn join() {
    assert_eq!(t("a\n  b\nc\n", "J"), "a b\nc\n");
    assert_eq!(t("a\n  b\nc\n", "3J"), "a b c\n");
    assert_eq!(t("a\n  b\n", "gJ"), "a  b\n");
    assert_eq!(t("a\n)\n", "J"), "a)\n");
    assert_eq!(t("a\n\nb\n", "J"), "a\nb\n");
}

#[test]
fn indent_ops() {
    assert_eq!(t("a\nb\n", ">>"), "  a\nb\n");
    assert_eq!(t("a\nb\n", ">j"), "  a\n  b\n");
    assert_eq!(t("a\nb\n", "2>>"), "  a\n  b\n");
    assert_eq!(t("    a\n", "<<"), "  a\n"); // one level (2 columns) at a time
    assert_eq!(t("    a\n", "<<<<"), "a\n");
    assert_eq!(t("  a\n", "<<"), "a\n");
    assert_eq!(t("a\n\nb\n", ">G"), "  a\n\n  b\n");
    assert_eq!(t("a\n", ">>>>"), "    a\n");
}

#[test]
fn case_ops() {
    assert_eq!(t("hello world\n", "gUiw"), "HELLO world\n");
    assert_eq!(t("hello world\n", "gUU"), "HELLO WORLD\n");
    assert_eq!(t("HELLO World\n", "guu"), "hello world\n");
    assert_eq!(t("Hello\n", "g~~"), "hELLO\n");
    assert_eq!(t("hello world\n", "gUw"), "HELLO world\n");
    assert_eq!(t("hello world\n", "g~iw"), "HELLO world\n");
}

#[test]
fn visual_mode() {
    assert_eq!(t("hello world\n", "vld"), "llo world\n");
    assert_eq!(t("hello world\n", "viwd"), " world\n");
    assert_eq!(t("hello world\n", "wviwd"), "hello \n");
    assert_eq!(t("hello world\n", "veey"), "hello world\n");
    assert_eq!(t("a\nb\nc\n", "Vjd"), "c\n");
    assert_eq!(t("a\nb\nc\n", "Vd"), "b\nc\n");
    assert_eq!(t("a\nb\nc\n", "Vj>"), "  a\n  b\nc\n");
    assert_eq!(t("hello\n", "vlU"), "HEllo\n");
    assert_eq!(t("hello\n", "vlrx"), "xxllo\n");
    assert_eq!(t("abc abc\n", "wviwcX<Esc>"), "abc X\n");
    assert_eq!(t("a b\n", "vey$p"), "a ba b\n"); // `e` from 'a' extends to the end of "b"
    assert_eq!(t("abc\n", "v$d"), "\n");
    assert_eq!(t("a\nb\n", "VjJ"), "a b\n");
    assert_eq!(t("ab\n", "vlo<Esc>x"), "b\n");
    assert_eq!(t("one two\n", "wvey0P"), "twoone two\n");
}

#[test]
fn visual_replace_with_register() {
    assert_eq!(t("foo bar\n", "yiwwviwp"), "foo foo\n");
    assert_eq!(t("foo bar\n", "yiwwviwp0P"), "barfoo foo\n"); // the replaced text lands in the unnamed register
}

#[test]
fn search() {
    assert_eq!(run("a b a b\n", "/b<CR>").1, (0, 2));
    assert_eq!(run("a b a b\n", "/b<CR>n").1, (0, 6));
    assert_eq!(run("a b a b\n", "/b<CR>nn").1, (0, 2)); // wraps
    assert_eq!(run("a b a b\n", "/b<CR>nN").1, (0, 2));
    assert_eq!(run("a b a b\n", "$?a<CR>").1, (0, 4));
    assert_eq!(t("foo bar foo\n", "d/bar<CR>"), "bar foo\n");
    assert_eq!(t("foo bar foo\n", "*x"), "foo bar oo\n");
    assert_eq!(t("foo bar foo\n", "$#x"), "oo bar foo\n"); // # searches backwards for the whole word under the cursor
    assert_eq!(run("x\nfoo\ny\nfoo\n", "j*").1, (3, 0));
    assert_eq!(run("x\nfoo\ny\nfoo\n", "G#").1, (1, 0)); // # from the second foo goes back to the first
    assert_eq!(run("x\nfoo\ny\nfoo\n", "jj/foo<CR>").1, (3, 0));
}

#[test]
fn substitute_command() {
    assert_eq!(t("a b a\n", ":s/a/X/<CR>"), "X b a\n");
    assert_eq!(t("a b a\n", ":s/a/X/g<CR>"), "X b X\n");
    assert_eq!(t("a\na\na\n", ":%s/a/b/<CR>"), "b\nb\nb\n");
    assert_eq!(t("a\na\na\n", ":2,3s/a/b/<CR>"), "a\nb\nb\n");
    assert_eq!(t("a\na\na\n", ":.,$s/a/b/<CR>"), "b\nb\nb\n");
    assert_eq!(t("a\na\na\n", "j:.,+1s/a/b/<CR>"), "a\nb\nb\n");
    assert_eq!(t("foo\n", ":s/o/0/g<CR>"), "f00\n");
    assert_eq!(t("foo\n", ":s/foo/[&]/<CR>"), "[foo]\n");
    assert_eq!(t("a/b\n", ":s#/#-#<CR>"), "a-b\n");
    assert_eq!(t("a,b\n", ":s/,/\\r/<CR>"), "a\nb\n");
    assert_eq!(t("Foo foo\n", ":s/foo/x/gi<CR>"), "x x\n");
    assert_eq!(t("a\na\n", ":%s/a/b/<CR>u"), "a\na\n");
}

#[test]
fn ex_range_commands() {
    assert_eq!(t("a\nb\nc\n", ":2d<CR>"), "a\nc\n");
    assert_eq!(t("a\nb\nc\n", ":1,2d<CR>"), "c\n");
    assert_eq!(t("a\nb\nc\n", ":$d<CR>"), "a\nb\n");
    assert_eq!(t("a\nb\nc\n", ":2y<CR>:$put<CR>"), "a\nb\nc\nb\n");
    assert_eq!(t("a\nb\nc\n", ":%j<CR>"), "a b c\n");
    assert_eq!(t("a\nb\n", ":%>\n".replace('\n', "<CR>").as_str()), "  a\n  b\n");
    assert_eq!(t("b\na\nc\n", ":sort<CR>"), "a\nb\nc\n");
    assert_eq!(t("b\na\nc\n", ":sort!<CR>"), "c\nb\na\n");
    assert_eq!(t("10\n9\n100\n", ":sort n<CR>"), "9\n10\n100\n");
    assert_eq!(run("a\nb\nc\n", ":3<CR>").1.0, 2);
    assert_eq!(run("a\nb\nc\n", ":$<CR>").1.0, 2);
    assert_eq!(t("a\nb\nc\n", ":2m0<CR>"), "b\na\nc\n");
    assert_eq!(t("a\nb\nc\n", ":1m$<CR>"), "b\nc\na\n");
    assert_eq!(t("a\nb\nc\n", ":1t.<CR>"), "a\na\nb\nc\n");
    assert_eq!(t("a\nb\nc\n", "jma:'a,$d<CR>"), "a\n");
}

#[test]
fn global_command() {
    assert_eq!(t("a1\nb\na2\n", ":g/a/d<CR>"), "b\n");
    assert_eq!(t("a1\nb\na2\n", ":v/a/d<CR>"), "a1\na2\n");
    assert_eq!(t("a1\nb\na2\n", ":g/a/s/a/X/<CR>"), "X1\nb\nX2\n");
    assert_eq!(t("a1\nb\na2\n", ":g/a/normal Ax<CR>"), "a1x\nb\na2x\n");
    assert_eq!(t("a1\nb\na2\n", ":g/a/d<CR>u"), "a1\nb\na2\n");
}

#[test]
fn macros() {
    assert_eq!(t("a\nb\nc\n", "qaAx<Esc>jq@a"), "ax\nbx\nc\n");
    assert_eq!(t("a\nb\nc\n", "qaAx<Esc>jq2@a"), "ax\nbx\ncx\n");
    assert_eq!(t("a\nb\nc\n", "qaAx<Esc>jq@a@@"), "ax\nbx\ncx\n");
    assert_eq!(t("1\n1\n1\n", "qq<C-a>jq2@q"), "2\n2\n2\n");
}

#[test]
fn marks_and_jumps() {
    assert_eq!(run("a\nb\nc\n", "majjd'a").0, "");
    // exclusive motion ending in column 0: the end backs up to the end of the previous line
    assert_eq!(run("abc\nd\n", "lmajd`a").0, "a\nd\n");
    assert_eq!(run("a\nb\nc\nd\n", "Gggx").1, (0, 0));
    assert_eq!(run("a\nb\nc\nd\n", "G<C-o>").1.0, 0);
    assert_eq!(run("a\nb\nc\nd\n", "G''").1.0, 0);
}

#[test]
fn increment_decrement() {
    assert_eq!(t("x 41\n", "<C-a>"), "x 42\n");
    assert_eq!(t("x 41\n", "5<C-a>"), "x 46\n");
    assert_eq!(t("x 41\n", "<C-x>"), "x 40\n");
    assert_eq!(t("x 0\n", "<C-x>"), "x -1\n");
    assert_eq!(t("x -1\n", "<C-a>"), "x 0\n");
    assert_eq!(t("x 9\n", "<C-a>"), "x 10\n");
}

#[test]
fn paragraphs_and_percent() {
    assert_eq!(t("a\nb\n\nc\n", "d}"), "\nc\n");
    assert_eq!(t("a\nb\n\nc\n", "jjd{"), "\nc\n");
    assert_eq!(t("f(a, b) x\n", "f(d%"), "f x\n");
    assert_eq!(t("f(a, (b)) x\n", "f(%x"), "f(a, (b) x\n");
}

#[test]
fn scrolling_does_not_panic() {
    let big: String = (0..500).map(|i| format!("line {}\n", i)).collect();
    let mut ed = editor(&big);
    feed(&mut ed, "<C-d><C-d><C-u><C-f><C-b><C-e><C-y>zzztzbGgg50GHML");
    assert!(ed.cursor().0 < 500);
}

#[test]
fn mode_changes() {
    let mut ed = editor("abc\n");
    feed(&mut ed, "i");
    assert_eq!(ed.mode, Mode::Insert);
    feed(&mut ed, "<Esc>");
    assert_eq!(ed.mode, Mode::Normal);
    feed(&mut ed, "v");
    assert_eq!(ed.mode, Mode::Visual);
    feed(&mut ed, "V");
    assert_eq!(ed.mode, Mode::VisualLine);
    feed(&mut ed, "<Esc>");
    assert_eq!(ed.mode, Mode::Normal);
    feed(&mut ed, ":");
    assert_eq!(ed.mode, Mode::Command);
    feed(&mut ed, "<Esc>");
    assert_eq!(ed.mode, Mode::Normal);
}

#[test]
fn cursor_after_escape_and_edge_cases() {
    assert_eq!(run("abc\n", "A<Esc>").1, (0, 2));
    assert_eq!(run("abc\n", "$").1, (0, 2));
    assert_eq!(run("abc\n", "10l").1, (0, 2));
    assert_eq!(run("a\n\nb\n", "j").1, (1, 0));
    assert_eq!(run("", "x").0, "");
    assert_eq!(run("", "dd").0, "");
    assert_eq!(run("", "p").0, "");
    assert_eq!(run("", "J").0, "");
    assert_eq!(run("a\n", "jjj").1, (0, 0));
    assert_eq!(run("a\n", "dw").0, "\n");
    assert_eq!(run("a\n", "ddp").0, "\na\n");
}

#[test]
fn quit_refuses_with_unsaved_changes() {
    let mut ed = editor("a\n");
    feed(&mut ed, "x:q<CR>");
    assert!(!ed.quit);
    feed(&mut ed, ":q!<CR>");
    assert!(ed.quit);
}

// ---- autopairs and Tab ---------------------------------------------------------------

/// Like `t`, but the buffer has a file name (so language-specific rules apply).
fn t_named(name: &str, text: &str, keys: &str) -> String {
    let mut ed = editor(text);
    ed.buf_mut().path = Some(std::path::PathBuf::from(name));
    feed(&mut ed, keys);
    ed.buf().text()
}

#[test]
fn autopairs_close_and_step_over() {
    assert_eq!(t("\n", "i(<Esc>"), "()\n");
    assert_eq!(t("\n", "i[{(<Esc>"), "[{()}]\n");
    assert_eq!(t("\n", "ifoo(a, b)<Esc>"), "foo(a, b)\n"); // typing the closer steps over it
    assert_eq!(t("\n", "ix = [1, (2)]<Esc>"), "x = [1, (2)]\n");
    assert_eq!(t("\n", "i\"hi\"<Esc>"), "\"hi\"\n");
    assert_eq!(t("\n", "if(x) { y }<Esc>"), "f(x) { y }\n");
    // the cursor sits between the pair, so more can be typed there
    assert_eq!(t("\n", "i(x<Esc>"), "(x)\n");
    // dot-repeat and counts keep working
    assert_eq!(t("a\nb\n", "A;<Esc>j.u"), "a;\nb\n");
}

#[test]
fn autopairs_only_when_the_next_char_allows() {
    assert_eq!(t("foo\n", "i(<Esc>"), "(foo\n"); // right before a word: no pair
    assert_eq!(t("x)\n", "i(<Esc>"), "(x)\n");
    assert_eq!(t("foo bar\n", "ea(<Esc>"), "foo() bar\n"); // before a space: pair
    assert_eq!(t("f()\n", "la{<Esc>"), "f({})\n"); // before a closer: pair
}

#[test]
fn autopairs_backspace_removes_an_empty_pair() {
    assert_eq!(t("\n", "i(<BS><Esc>"), "\n");
    assert_eq!(t("\n", "i{<BS><Esc>"), "\n");
    assert_eq!(t("\n", "i\"<BS><Esc>"), "\n");
    assert_eq!(t("\n", "i(x<BS><BS><Esc>"), "\n");
    assert_eq!(t("\n", "i(x<BS><Esc>"), "()\n");
    // not an empty pair any more: a normal backspace
    assert_eq!(t("\n", "i(x)<Left><BS><Esc>"), "()\n");
}

#[test]
fn autopairs_enter_opens_a_block() {
    assert_eq!(t("\n", "i{<CR>x<Esc>"), "{\n  x\n}\n");
    assert_eq!(t("\n", "ifn f() {<CR>x<Esc>"), "fn f() {\n  x\n}\n");
    assert_eq!(t("\n", "if(<CR>a,<Esc>"), "f(\n  a,\n)\n");
}

#[test]
fn autopairs_quotes_are_context_aware() {
    assert_eq!(t("\n", "idon't<Esc>"), "don't\n"); // apostrophe after a word
    assert_eq!(t("\n", "i'a'<Esc>"), "'a'\n");
    assert_eq!(t("\n", "i`x`<Esc>"), "`x`\n");
    assert_eq!(t("\n", "ix = \"a \\\"b\\\" c\"<Esc>"), "x = \"a \\\"b\\\" c\"\n"); // escaped quotes
    // string prefixes still pair
    assert_eq!(t_named("a.py", "\n", "ix = f\"hi {name}\"<Esc>"), "x = f\"hi {name}\"\n");
    assert_eq!(t_named("a.py", "\n", "ix = r'a'<Esc>"), "x = r'a'\n");
    // a bracket typed directly inside a string pairs, and closes again
    assert_eq!(t("\n", "i\"{<Esc>"), "\"{}\"\n");
    // Rust lifetimes are not char literals
    assert_eq!(t_named("a.rs", "\n", "ifn f(x: &'a str) {}<Esc>"), "fn f(x: &'a str) {}\n");
    assert_eq!(t_named("a.rs", "\n", "iimpl<'a> X<Esc>"), "impl<'a> X\n");
    assert_eq!(t_named("a.rs", "\n", "ilet c = 'x';<Esc>"), "let c = 'x';\n");
}

#[test]
fn autopairs_python_docstrings() {
    assert_eq!(t_named("a.py", "\n", "i\"\"\"<Esc>"), "\"\"\"\"\"\"\n");
    assert_eq!(t_named("a.py", "\n", "i\"\"\"doc\"\"\"<Esc>"), "\"\"\"doc\"\"\"\n");
    // elsewhere three quotes are just an empty string plus a pair
    assert_eq!(t("\n", "i\"\"<Esc>"), "\"\"\n");
}

#[test]
fn autopairs_can_be_turned_off() {
    let mut ed = editor("\n");
    ed.opts.autopairs = false;
    feed(&mut ed, "i(\"x<Esc>");
    assert_eq!(ed.buf().text(), "(\"x\n");
    let mut ed = editor("\n");
    feed(&mut ed, ":set noautopairs<CR>i(<Esc>");
    assert_eq!(ed.buf().text(), "(\n");
    // pasted text is never touched
    let mut ed = editor("\n");
    ed.handle_key(Key::Char('i'));
    ed.handle_key(Key::Paste("f(\"a\"".into()));
    assert_eq!(ed.buf().text(), "f(\"a\"\n");
}

#[test]
fn tab_is_always_two_spaces() {
    assert_eq!(t("\n", "i<Tab>x<Esc>"), "  x\n");
    assert_eq!(t("ab\n", "a<Tab>x<Esc>"), "a  xb\n"); // a full 2, not "up to the next tab stop"
    assert_eq!(t("ab\n", "la<Tab>x<Esc>"), "ab  x\n");
    assert_eq!(t("abc\n", "a<Tab>x<Esc>"), "a  xbc\n");
    assert_eq!(t("\n", "i<Tab><Tab>x<Esc>"), "    x\n");
    assert_eq!(t("a\n", ">>"), "  a\n");
    assert_eq!(t("a\n", "i<Tab>x<C-d><Esc>"), "xa\n");
}

#[test]
fn the_indent_width_is_configurable() {
    let mut ed = editor("a\n");
    ed.opts.shiftwidth = 4;
    ed.opts.tabstop = 4;
    feed(&mut ed, "i<Tab>x<Esc>>>");
    assert_eq!(ed.buf().text(), "        xa\n");
    // real tabs: one level is one tab (tabstop columns wide), whatever shiftwidth says
    let mut ed = editor("a\n");
    ed.opts.expandtab = false;
    feed(&mut ed, "i<Tab>x<Esc>>>");
    assert_eq!(ed.buf().text(), "\t\txa\n");
    feed(&mut ed, "<<");
    assert_eq!(ed.buf().text(), "\txa\n");
}

fn temp_file(name: &str, text: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("ved-vim-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join(name);
    std::fs::write(&p, text).unwrap();
    p
}

#[test]
fn tab_ignores_the_files_own_indentation_unless_asked() {
    let two = temp_file("four.rs", "fn f() {\n    a;\n    b;\n}\n");
    let mut ed = editor("\n");
    ed.docs[0].buf = ved::buffer::Buffer::open(&two).unwrap();
    feed(&mut ed, "jI<Tab><Esc>");
    assert_eq!(ed.buf().line_string(1), "      a;"); // 2 spaces added to the 4 that were there
    // ... and opting in follows the file
    let mut ed = editor("\n");
    ed.opts.detectindent = true;
    ed.docs[0].buf = ved::buffer::Buffer::open(&two).unwrap();
    feed(&mut ed, "jI<Tab><Esc>");
    assert_eq!(ed.buf().line_string(1), "        a;"); // 4 spaces, like the file
}

#[test]
fn makefiles_keep_real_tabs() {
    let mk = temp_file("Makefile", "all:\n");
    let mut ed = editor("\n");
    ed.docs[0].buf = ved::buffer::Buffer::open(&mk).unwrap();
    feed(&mut ed, "Go<Tab>echo x<Esc>");
    assert_eq!(ed.buf().text(), "all:\n\techo x\n");
}

#[test]
fn autopairs_closer_on_the_fresh_block_line_takes_the_waiting_one() {
    assert_eq!(t("\n", "ifn f() {<CR>}<Esc>"), "fn f() {\n}\n");
    assert_eq!(t("\n", "if(<CR>)<Esc>"), "f(\n)\n");
    assert_eq!(t("\n", "ifn f() {<CR>}<CR>x<Esc>"), "fn f() {\n}\nx\n");
    // once something was typed on the line, a closer is a plain character again
    assert_eq!(t("\n", "i{<CR>x}<Esc>"), "{\n  x}\n}\n");
    // nested blocks
    assert_eq!(t("\n", "iif a {<CR>if b {<CR>}<CR>}<Esc>"), "if a {\n  if b {\n  }\n}\n");
}

// ---- completion popup keys ---------------------------------------------------------------

fn show_popup(ed: &mut Editor, labels: &[&str], start_col: usize) {
    let items = labels
        .iter()
        .map(|l| ved::lsp::CompItem { label: l.to_string(), insert: l.to_string(), filter: l.to_string(), sort: l.to_string(), ..Default::default() })
        .collect();
    ed.completion = Some(ved::editor::Completion { items, shown: (0..labels.len()).collect(), sel: 0, start_col });
}

/// Type `typed` after the `x` on the first line, open a popup, then feed `keys`.
fn with_popup(typed: &str, labels: &[&str], keys: &str) -> (Editor, String) {
    let mut ed = editor("x\ny\n");
    feed(&mut ed, &format!("A{}", typed));
    show_popup(&mut ed, labels, 1);
    feed(&mut ed, keys);
    let text = ed.buf().text();
    (ed, text)
}

#[test]
fn completion_enter_accepts_the_highlighted_option() {
    let (ed, text) = with_popup("fo", &["foo", "foobar", "fob"], "<CR>");
    assert_eq!(text, "xfoo\ny\n"); // no newline was inserted
    assert!(ed.completion.is_none());
    assert_eq!(ed.mode, Mode::Insert);
}

#[test]
fn completion_tab_and_shift_tab_move_the_highlight() {
    assert_eq!(with_popup("fo", &["foo", "foobar", "fob"], "<Tab><CR>").1, "xfoobar\ny\n");
    assert_eq!(with_popup("fo", &["foo", "foobar", "fob"], "<Tab><Tab><CR>").1, "xfob\ny\n");
    assert_eq!(with_popup("fo", &["foo", "foobar", "fob"], "<Tab><Tab><Tab><CR>").1, "xfoo\ny\n"); // wraps
    assert_eq!(with_popup("fo", &["foo", "foobar", "fob"], "<S-Tab><CR>").1, "xfob\ny\n"); // up from the top wraps
    assert_eq!(with_popup("fo", &["foo", "foobar", "fob"], "<Tab><Tab><S-Tab><CR>").1, "xfoobar\ny\n");
    // Tab only moves: the popup stays open and nothing is inserted
    let (ed, text) = with_popup("fo", &["foo", "foobar"], "<Tab>");
    assert_eq!(text, "xfo\ny\n");
    assert!(ed.completion.is_some());
    // the old keys still work
    assert_eq!(with_popup("fo", &["foo", "foobar"], "<C-n><C-y>").1, "xfoobar\ny\n");
    assert_eq!(with_popup("fo", &["foo", "foobar"], "<Down><CR>").1, "xfoobar\ny\n");
    assert_eq!(with_popup("fo", &["foo", "foobar"], "<Down><Up><CR>").1, "xfoo\ny\n");
}

#[test]
fn completion_tab_still_indents_without_a_popup() {
    assert_eq!(t("\n", "i<Tab>x<Esc>"), "  x\n");
    assert_eq!(t("\n", "ia<S-Tab><Esc>"), "a\n"); // Shift-Tab dedents as before
}

#[test]
fn completion_enter_is_a_newline_when_the_word_is_complete() {
    let (ed, text) = with_popup("foo", &["foo", "foobar"], "<CR>");
    assert_eq!(text, "xfoo\n\ny\n");
    assert!(ed.completion.is_none());
    // ... but once another option is highlighted it does accept
    assert_eq!(with_popup("foo", &["foo", "foobar"], "<Tab><CR>").1, "xfoobar\ny\n");
}

#[test]
fn completion_ctrl_e_dismisses_and_esc_leaves_insert() {
    let (ed, text) = with_popup("fo", &["foo"], "<C-e>");
    assert_eq!(text, "xfo\ny\n");
    assert!(ed.completion.is_none());
    assert_eq!(ed.mode, Mode::Insert);
    let (ed, _) = with_popup("fo", &["foo"], "<Esc>");
    assert!(ed.completion.is_none());
    assert_eq!(ed.mode, Mode::Normal);
}

#[test]
fn accepted_completions_replay_with_dot_and_macros() {
    // dot-repeat re-types the result instead of replaying Tab / Enter
    let mut ed = editor("x\ny\n");
    feed(&mut ed, "Afo");
    show_popup(&mut ed, &["foo", "foobar"], 1);
    feed(&mut ed, "<Tab><CR><Esc>j.");
    assert_eq!(ed.buf().text(), "xfoobar\nyfoobar\n");
    // so does a macro
    let mut ed = editor("x\ny\n");
    feed(&mut ed, "qaAfo");
    show_popup(&mut ed, &["foo", "foobar"], 1);
    feed(&mut ed, "<Tab><CR><Esc>q");
    feed(&mut ed, "j@a");
    assert_eq!(ed.buf().text(), "xfoobar\nyfoobar\n");
}

// ---- LSP snippets ------------------------------------------------------------------------

fn snippet_item(label: &str, filter: &str, raw: &str) -> ved::lsp::CompItem {
    ved::lsp::CompItem {
        label: label.to_string(),
        insert: ved::snippet::parse_snippet(raw).text,
        filter: filter.to_string(),
        sort: label.to_string(),
        snippet: Some(raw.to_string()),
        ..Default::default()
    }
}

/// Open `text`, type `typed` at the end of the first line, show a popup with `items`, then feed `keys`.
fn with_snippets(text: &str, typed: &str, items: Vec<ved::lsp::CompItem>, start_col: usize, keys: &str) -> Editor {
    let mut ed = editor(text);
    feed(&mut ed, &format!("A{}", typed));
    let n = items.len();
    ed.completion = Some(ved::editor::Completion { items, shown: (0..n).collect(), sel: 0, start_col });
    feed(&mut ed, keys);
    ed
}

fn call_item() -> ved::lsp::CompItem {
    snippet_item("foo(int a, int b)", "foo", "foo(${1:a}, ${2:b})$0")
}

fn include_items() -> Vec<ved::lsp::CompItem> {
    vec![
        snippet_item("include \"header\"", "include", "include \"${1:header}\""),
        snippet_item("include <header>", "include", "include <${1:header}>"),
    ]
}

#[test]
fn snippet_include_inserts_the_whole_directive() {
    // the case from the bug report: `#inc`, Tab to `include <header>`, Enter
    let mut ed = with_snippets("\n", "#inc", include_items(), 1, "<Tab><CR>");
    assert_eq!(ed.buf().text(), "#include <header>\n");
    assert_eq!(ed.cursor(), (0, 10));
    assert_eq!(ed.snippet_selection(), Some((0, 10, 16))); // `header` is highlighted
    // typing replaces the placeholder
    feed(&mut ed, "stdio.h");
    assert_eq!(ed.buf().text(), "#include <stdio.h>\n");
    // Tab leaves the snippet, after the closing `>`
    feed(&mut ed, "<Tab>");
    assert_eq!(ed.cursor(), (0, 18));
    assert!(ed.snippet.is_none());
    feed(&mut ed, "<CR>int x;<Esc>");
    assert_eq!(ed.buf().text(), "#include <stdio.h>\nint x;\n");
    // the quoted form
    let ed = with_snippets("\n", "#inc", include_items(), 1, "<CR>");
    assert_eq!(ed.buf().text(), "#include \"header\"\n");
    assert_eq!(ed.snippet_selection(), Some((0, 10, 16)));
}

#[test]
fn snippet_function_call_walks_through_the_arguments() {
    let mut ed = with_snippets("\n", "fo", vec![call_item()], 0, "<CR>");
    assert_eq!(ed.buf().text(), "foo(a, b)\n");
    assert_eq!(ed.snippet_selection(), Some((0, 4, 5)));
    feed(&mut ed, "1<Tab>");
    assert_eq!(ed.snippet_selection(), Some((0, 7, 8)));
    feed(&mut ed, "22<Tab>");
    assert!(ed.snippet.is_none()); // `$0` reached
    feed(&mut ed, ";<Esc>");
    assert_eq!(ed.buf().text(), "foo(1, 22);\n");
}

#[test]
fn snippet_shift_tab_goes_back_and_reselects() {
    let mut ed = with_snippets("\n", "fo", vec![call_item()], 0, "<CR>xy<Tab>");
    assert_eq!(ed.buf().text(), "foo(xy, b)\n");
    assert_eq!(ed.snippet_selection(), Some((0, 8, 9)));
    feed(&mut ed, "<S-Tab>");
    // back on the first one, whose typed text is selected again
    assert_eq!(ed.snippet_selection(), Some((0, 4, 6)));
    feed(&mut ed, "Z<Tab><Tab>;");
    assert_eq!(ed.buf().text(), "foo(Z, b);\n");
    // Shift-Tab on the first placeholder stays there
    let mut ed = with_snippets("\n", "fo", vec![call_item()], 0, "<CR><S-Tab>");
    assert_eq!(ed.snippet_selection(), Some((0, 4, 5)));
    feed(&mut ed, "q");
    assert_eq!(ed.buf().text(), "foo(q, b)\n");
}

#[test]
fn snippet_backspace_clears_the_placeholder_and_edits_are_tracked() {
    let mut ed = with_snippets("\n", "fo", vec![call_item()], 0, "<CR><BS>");
    assert_eq!(ed.buf().text(), "foo(, b)\n");
    assert!(ed.snippet.is_some());
    feed(&mut ed, "z<Tab>");
    assert_eq!(ed.snippet_selection(), Some((0, 7, 8)));
    // backspace inside what was typed shifts the later placeholders back
    let mut ed = with_snippets("\n", "fo", vec![call_item()], 0, "<CR>xyz<BS><Tab>");
    assert_eq!(ed.buf().text(), "foo(xy, b)\n");
    assert_eq!(ed.snippet_selection(), Some((0, 8, 9)));
    feed(&mut ed, "<Esc>");
    assert_eq!(ed.mode, Mode::Normal);
    assert!(ed.snippet.is_none());
}

#[test]
fn snippet_escape_keeps_the_default_text_and_ends_the_session() {
    let mut ed = with_snippets("\n", "fo", vec![call_item()], 0, "<CR><Esc>");
    assert_eq!(ed.buf().text(), "foo(a, b)\n");
    assert_eq!(ed.mode, Mode::Normal);
    assert!(ed.snippet.is_none());
    // a later insert starts clean: Tab is an ordinary 2-space indent again
    feed(&mut ed, "A<Tab>x<Esc>");
    assert_eq!(ed.buf().text(), "foo(a, b)  x\n");
    // moving the cursor with the arrows also ends it
    let mut ed = with_snippets("\n", "fo", vec![call_item()], 0, "<CR><Left>");
    assert!(ed.snippet.is_none());
    feed(&mut ed, "<Tab>");
    assert_eq!(ed.buf().text(), "foo  (a, b)\n"); // the cursor moved to column 3: Tab indents there
}

#[test]
fn snippet_enter_inside_a_placeholder_keeps_the_others() {
    let mut ed = with_snippets("\n", "fo", vec![call_item()], 0, "<CR>x<CR>");
    assert_eq!(ed.buf().text(), "foo(x\n, b)\n");
    feed(&mut ed, "<Tab>");
    assert_eq!(ed.snippet_selection(), Some((1, 2, 3))); // `b` on its new row
    feed(&mut ed, "y");
    assert_eq!(ed.buf().text(), "foo(x\n, y)\n");
}

#[test]
fn snippet_works_with_autopairs() {
    let mut ed = with_snippets("\n", "fo", vec![snippet_item("foo(arg)", "foo", "foo(${1:arg})$0")], 0, "<CR>\"hi");
    assert_eq!(ed.buf().text(), "foo(\"hi\")\n");
    feed(&mut ed, "<Tab>;<Esc>");
    assert_eq!(ed.buf().text(), "foo(\"hi\");\n"); // Tab jumped over the quote and the paren
}

#[test]
fn snippet_multi_line_follows_the_indentation() {
    let item = snippet_item("if", "if", "if (${1:cond}) {\n\t$0\n}");
    let mut ed = with_snippets("    i\n", "f", vec![item], 4, "<CR>");
    assert_eq!(ed.buf().text(), "    if (cond) {\n      \n    }\n");
    assert_eq!(ed.snippet_selection(), Some((0, 8, 12)));
    feed(&mut ed, "a<lt>b<Tab>");
    assert_eq!(ed.cursor(), (1, 6)); // `$0`, inside the block
    feed(&mut ed, "x<Esc>");
    assert_eq!(ed.buf().text(), "    if (a<b) {\n      x\n    }\n");
}

#[test]
fn snippet_tabs_become_the_indent_unit() {
    // buffers that use real tabs get real tabs
    let mut opts = Options::default();
    opts.osc52 = false;
    opts.lsp = false;
    opts.expandtab = false;
    let mut ed = Editor::new(opts, 24, 80);
    ed.docs[0].buf = ved::buffer::Buffer::from_text("\t\n");
    feed(&mut ed, "Aw");
    let item = snippet_item("while", "w", "while (${1:c}) {\n\t$0\n}");
    ed.completion = Some(ved::editor::Completion { items: vec![item], shown: vec![0], sel: 0, start_col: 1 });
    feed(&mut ed, "<CR>");
    assert_eq!(ed.buf().text(), "\twhile (c) {\n\t\t\n\t}\n");
}

#[test]
fn snippet_dot_repeat_replays_the_plain_text() {
    let mut ed = with_snippets("x\ny\n", "fo", vec![call_item()], 1, "<CR>1<Tab>2<Tab>;<Esc>");
    assert_eq!(ed.buf().text(), "xfoo(1, 2);\ny\n");
    feed(&mut ed, "j.");
    // the typed values are not replayed, the snippet's own text is
    assert_eq!(ed.buf().text(), "xfoo(1, 2);\nyfoo(a, b);\n");
}

#[test]
fn snippet_plain_completion_inside_a_placeholder_keeps_tracking() {
    let mut ed = with_snippets("\n", "fo", vec![call_item()], 0, "<CR>x");
    // a word completion accepted while filling in the first argument
    ed.completion = Some(ved::editor::Completion {
        items: vec![ved::lsp::CompItem { label: "xylophone".into(), insert: "xylophone".into(), filter: "xylophone".into(), sort: "a".into(), ..Default::default() }],
        shown: vec![0],
        sel: 0,
        start_col: 4,
    });
    feed(&mut ed, "<CR>");
    assert_eq!(ed.buf().text(), "foo(xylophone, b)\n");
    feed(&mut ed, "<Tab>");
    assert_eq!(ed.snippet_selection(), Some((0, 15, 16)));
}

#[test]
fn snippet_without_placeholders_just_puts_the_cursor_at_the_end() {
    // a snippet item without any `$` is plain text; `$0` alone sets the cursor
    let mut ed = with_snippets("\n", "pr", vec![snippet_item("print()", "print", "print($0)")], 0, "<CR>");
    assert_eq!(ed.buf().text(), "print()\n");
    assert_eq!(ed.cursor(), (0, 6));
    assert!(ed.snippet.is_none());
    feed(&mut ed, "1<Esc>");
    assert_eq!(ed.buf().text(), "print(1)\n");
    let ed = with_snippets("\n", "pr", vec![snippet_item("print", "print", "print")], 0, "<CR>");
    assert_eq!(ed.cursor(), (0, 5));
}

#[test]
fn snippet_is_undone_in_one_step() {
    let mut ed = with_snippets("\n", "fo", vec![call_item()], 0, "<CR>1<Tab>2<Tab><Esc>");
    assert_eq!(ed.buf().text(), "foo(1, 2)\n");
    feed(&mut ed, "u");
    assert_eq!(ed.buf().text(), "\n");
}

// ---- #include completion (the clangd flow: `#include <>` first, header names after) -------------

fn path_item(name: &str) -> ved::lsp::CompItem {
    ved::lsp::CompItem { label: name.to_string(), insert: name.to_string(), filter: name.to_string(), sort: name.to_string(), ..Default::default() }
}

/// `text` with the cursor right after its first `open`, in insert mode; then `typed`, a popup, `keys`.
fn include_flow(text: &str, open: &str, typed: &str, items: &[&str], start_col: usize, keys: &str) -> Editor {
    let mut ed = editor(text);
    feed(&mut ed, &format!("0f{}a{}", open, typed));
    let items: Vec<_> = items.iter().map(|n| path_item(n)).collect();
    let n = items.len();
    ed.completion = Some(ved::editor::Completion { items, shown: (0..n).collect(), sel: 0, start_col });
    feed(&mut ed, keys);
    ed
}

#[test]
fn include_header_does_not_double_the_closing_bracket() {
    // the bug report: the snippet left `#include <>`, then `stdio.h>` from the popup made `<stdio.h>>`
    let ed = include_flow("#include <>\n", "<lt>", "std", &["stdio.h>", "stdio_ext.h>"], 10, "<CR>");
    assert_eq!(ed.buf().text(), "#include <stdio.h>\n");
    assert_eq!(ed.cursor(), (0, 18));
    assert_eq!(ed.mode, Mode::Insert);
    // the second entry too, and a header picked with the arrows
    let ed = include_flow("#include <>\n", "<lt>", "std", &["stdio.h>", "stdio_ext.h>"], 10, "<Tab><CR>");
    assert_eq!(ed.buf().text(), "#include <stdio_ext.h>\n");
    // the quoted form
    let ed = include_flow("#include \"\"\n", "\"", "my", &["mylib.h\""], 10, "<CR>");
    assert_eq!(ed.buf().text(), "#include \"mylib.h\"\n");
    // other spellings of the directive, and indentation before the `#`
    let ed = include_flow("  #  include_next <>\n", "<lt>", "st", &["stdio.h>"], 19, "<CR>");
    assert_eq!(ed.buf().text(), "  #  include_next <stdio.h>\n");
    // a directory has no closer: the `>` stays where it is
    let ed = include_flow("#include <>\n", "<lt>", "sy", &["sys/"], 10, "<CR>");
    assert_eq!(ed.buf().text(), "#include <sys/>\n");
    assert_eq!(ed.cursor(), (0, 14));
}

#[test]
fn include_header_after_a_directory_part() {
    let ed = include_flow("#include <>\n", "<lt>", "sys/ty", &["types.h>"], 14, "<CR>");
    assert_eq!(ed.buf().text(), "#include <sys/types.h>\n");
}

#[test]
fn closing_brackets_are_only_merged_on_include_lines() {
    // anywhere else the following `>` belongs to something else and stays
    let ed = include_flow("vec<>\n", "<lt>", "in", &["int>"], 4, "<CR>");
    assert_eq!(ed.buf().text(), "vec<int>>\n");
    // a `#include` line whose bracket is already closed before the cursor is left alone as well
    let ed = include_flow("#include <a.h> x>\n", "x", "", &["y>"], 15, "<CR>");
    assert_eq!(ed.buf().text(), "#include <a.h> y>>\n");
}

#[test]
fn typing_the_closing_bracket_steps_over_it_on_include_lines() {
    assert_eq!(t("#include <>\n", "0f<astdio.h>x<Esc>"), "#include <stdio.h>x\n");
    // the quote form already did
    assert_eq!(t("#include \"\"\n", "0f\"afoo.h\"x<Esc>"), "#include \"foo.h\"x\n");
    // elsewhere `>` is just a character
    assert_eq!(t("a<>\n", "0f<a>x<Esc>"), "a<>x>\n");
    assert_eq!(t("x = 1\n", "A>><Esc>"), "x = 1>>\n");
}

#[test]
fn accepting_a_directory_asks_for_its_contents() {
    let mut ed = include_flow("#include <>\n", "<lt>", "sy", &["sys/", "syslog.h>"], 10, "<CR>");
    assert_eq!(ed.buf().text(), "#include <sys/>\n");
    assert!(ed.lsp.comp_due.is_some(), "the list inside the directory should be requested");
    // a plain header does not
    ed = include_flow("#include <>\n", "<lt>", "sy", &["syslog.h>"], 10, "<CR>");
    assert!(ed.lsp.comp_due.is_none());
}

// ---- theme option ------------------------------------------------------------------------

#[test]
fn theme_option_defaults_to_dark_and_parses() {
    use ved::config::Theme;
    let mut o = Options::default();
    assert_eq!(o.theme, Theme::Dark);
    o.set("theme=terminal").unwrap();
    assert_eq!(o.theme, Theme::Terminal);
    assert_eq!(o.describe("theme").unwrap(), "theme=terminal");
    o.set("background=dark").unwrap(); // vim's spelling works too
    assert_eq!(o.theme, Theme::Dark);
    assert!(o.set("theme=light").unwrap_err().contains("no light theme"));
    assert!(o.set("theme=neon").unwrap_err().contains("unknown theme"));
    assert_eq!(o.theme, Theme::Dark);
}

#[test]
fn dark_theme_only_replaces_default_colours() {
    use ved::render::{apply_dark, THEME_BG, THEME_FG};
    use ved::screen::{Color, Frame, Style};
    let mut f = Frame::new(2, 3);
    f.set(0, 0, 'a', Style::fg(Color::Idx(5)));
    f.set(0, 1, 'b', Style::DEFAULT.with_bg(Color::Idx(24)));
    apply_dark(&mut f);
    assert_eq!(f.get(0, 0).style, Style { fg: Color::Idx(5), bg: THEME_BG, attrs: 0 });
    assert_eq!(f.get(0, 1).style, Style { fg: THEME_FG, bg: Color::Idx(24), attrs: 0 });
    assert_eq!(f.get(1, 2).style, Style { fg: THEME_FG, bg: THEME_BG, attrs: 0 });
}

#[test]
fn terminal_background_sequences() {
    use ved::render::{THEME_BG, THEME_BG_RGB};
    use ved::screen::Color;
    use ved::sys::{default_bg_seq, term_takes_bg};
    // the RGB value really is the colour the cells use (xterm's grey ramp)
    let Color::Idx(n) = THEME_BG else { panic!("theme background is an indexed colour") };
    let v = 8 + 10 * (n - 232);
    assert_eq!(THEME_BG_RGB, (v, v, v));
    assert_eq!(default_bg_seq(Some(THEME_BG_RGB)), "\x1b]11;rgb:1c/1c/1c\x1b\\");
    assert_eq!(default_bg_seq(None), "\x1b]111\x1b\\");
    // terminals that would print the sequence as text are left alone
    assert!(term_takes_bg(Some("xterm-256color")));
    assert!(term_takes_bg(Some("foot")));
    assert!(term_takes_bg(Some("tmux-256color")));
    assert!(!term_takes_bg(Some("linux")));
    assert!(!term_takes_bg(Some("dumb")));
    assert!(!term_takes_bg(Some("")));
    assert!(!term_takes_bg(None));
}

#[test]
fn termbg_option() {
    let mut o = Options::default();
    assert!(o.termbg);
    o.set("notermbg").unwrap();
    assert!(!o.termbg);
    o.set("tbg").unwrap();
    assert!(o.termbg);
}

// ---- regressions found by tests/fuzz.rs: none of these may panic, hang or eat memory ----------

#[test]
fn command_line_word_delete_respects_character_boundaries() {
    // ctrl-w on non-ASCII text in the `:` line used to cut inside a character
    for keys in [":é<C-w>", ":héllo wörld<C-w><C-w><C-w>", ":日本語 テスト<C-w>x<CR>", "/ünï<C-w><C-w><CR>", ":a  <C-w>b<CR>"] {
        let _ = run("text\n", keys);
    }
    let (_, (r, c)) = run("abc\n", ":é<C-w>ix<Esc>");
    assert_eq!((r, c), (0, 0));
}

#[test]
fn insert_ctrl_t_and_ctrl_d_never_underflow() {
    assert_eq!(t("x\n", "i<C-t><Esc>"), "  x\n");
    assert_eq!(t("x\n", "i<C-t><C-t><C-d><Esc>"), "  x\n");
    assert_eq!(t("x\n", "i<C-d><C-d><C-d><Esc>"), "x\n");
    assert_eq!(t("   x\n", "A<C-d><C-d><C-d><Esc>"), "x\n");
    let _ = run("\n", "i<C-t><C-d><C-d><C-t><Esc>");
    // a line shorter than the shiftwidth, with the cursor anywhere
    let _ = run("é\n", "$i<C-t><C-t><C-t><C-d><Esc>");
}

#[test]
fn huge_repeat_counts_finish_quickly_and_stay_bounded() {
    let start = std::time::Instant::now();
    // a big count is honoured when it is reasonable...
    let big = t("\n", "100000ix<Esc>");
    assert_eq!(big.len(), 100_001);
    // ...and cut short when it is absurd: no hang, no out-of-memory
    let _ = run("a\n", "99999999999ix<Esc>");
    let _ = run("a\n", "99999999999999999999x");
    let _ = run("a b c\n", "yy99999999999p");
    let _ = run("a\nb\n", "99999999999dd");
    let _ = run("a b\n", "99999999999.");
    let _ = run("a b\n", "ix<Esc>99999999999@q");
    assert!(start.elapsed() < std::time::Duration::from_secs(60), "took {:?}", start.elapsed());
}

#[test]
fn a_command_that_replays_the_dot_register_does_not_recurse_forever() {
    // `.` repeating a change whose own keys contain `.` (via a macro) used to overflow the stack
    let _ = run("a b c d\n", "ix<Esc>qq.q@q@q@q");
    let _ = run("a\n", "qqix<Esc>@qq@q");
    let _ = run("a\n", "qq@qq@q@q");
    // the plain case still works
    assert_eq!(t("a\n", "ix<Esc>.."), "xxxa\n");
}

#[test]
fn ex_addresses_and_numbers_saturate_instead_of_overflowing() {
    for cmd in [
        ":99999999999999999999d<CR>",
        ":1,99999999999999999999d<CR>",
        ":99999999999999999999,1d<CR>",
        ":.+99999999999999999999d<CR>",
        ":.-99999999999999999999d<CR>",
        ":99999999999999999999s/a/b/<CR>",
        ":g/a/normal 99999999999999999999x<CR>",
        ":99999999999999999999<CR>",
        ":0<CR>",
        ":-1<CR>",
    ] {
        let _ = run("a\nb\nc\n", cmd);
    }
}

#[test]
fn a_tiny_terminal_never_breaks_rendering_or_popups() {
    for (rows, cols) in [(1usize, 1usize), (1, 80), (2, 5), (3, 3), (5, 11), (6, 12), (24, 1)] {
        let mut opts = Options::default();
        opts.osc52 = false;
        opts.lsp = false;
        let mut ed = Editor::new(opts, rows, cols);
        ed.docs[0].buf = ved::buffer::Buffer::from_text("héllo wörld\n日本語\n\tx\n");
        let mut frame = ved::screen::Frame::new(rows, cols);
        for keys in ["ihello<Esc>", ":set list<CR>", "/w<CR>", ":e .<CR>", "<Esc>", "ix<C-n>", "<Esc>"] {
            feed(&mut ed, keys);
            ed.draw(&mut frame);
        }
    }
}
