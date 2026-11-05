#![allow(clippy::field_reassign_with_default)]
//! Indent guides as drawn on the screen (the arithmetic is unit-tested in src/guides.rs).

use ved::buffer::Buffer;
use ved::config::Options;
use ved::editor::Editor;
use ved::input::Key;
use ved::screen::{Color, Frame};

const GUIDE: Color = Color::Idx(238);
const SCOPE: Color = Color::Idx(245);

fn editor(text: &str) -> Editor {
    let mut opts = Options::default();
    opts.osc52 = false;
    opts.lsp = false;
    opts.sidebar = false;
    opts.shiftwidth = 4;
    opts.tabstop = 4;
    let mut ed = Editor::new(opts, 24, 80);
    ed.docs[0].buf = Buffer::from_text(text);
    ed
}

fn keys(ed: &mut Editor, s: &str) {
    for c in s.chars() {
        ed.handle_key(match c {
            '\r' => Key::Enter,
            '\x1b' => Key::Esc,
            c => Key::Char(c),
        });
    }
}

struct Shot {
    f: Frame,
    gw: usize,
    bar: usize,
}

impl Shot {
    fn new(ed: &mut Editor) -> Shot {
        let mut f = Frame::new(24, 80);
        ed.draw(&mut f);
        Shot { f, gw: ed.gutter_width(), bar: ed.bar_rows() }
    }
    /// The cell of text column `c` on buffer line `l` (the window has not scrolled).
    fn cell(&self, l: usize, c: usize) -> ved::screen::Cell {
        self.f.get(self.bar + l, self.gw + c)
    }
    fn guide(&self, l: usize, c: usize) -> bool {
        self.cell(l, c).ch == '\u{2502}'
    }
}

const CODE: &str = "fn a() {\n    x;\n\n    if y {\n        z;\n    }\n}\n";

#[test]
fn guides_are_drawn_in_the_indentation_and_carry_on_through_blank_lines() {
    let mut ed = editor(CODE);
    let s = Shot::new(&mut ed);
    assert!(!s.guide(0, 0), "no guide on a line at the top level");
    assert!(s.guide(1, 0), "one level: one guide");
    assert!(!s.guide(1, 4), "the text itself is where the next level would be");
    assert_eq!(s.cell(1, 4).ch, 'x');
    assert!(s.guide(2, 0), "the blank line inside the block keeps its guide");
    assert!(s.guide(3, 0) && !s.guide(3, 4));
    assert!(s.guide(4, 0) && s.guide(4, 4), "two levels: two guides");
    assert_eq!(s.cell(4, 8).ch, 'z');
    assert!(s.guide(5, 0));
    assert!(!s.guide(6, 0), "the closing brace is back at the top level");
    // below the end of the text there is nothing
    assert!(!s.guide(9, 0));
    // the gutter and the text were not touched
    assert!(!(0..s.gw).any(|c| s.f.get(s.bar + 1, c).ch == '\u{2502}'));
}

#[test]
fn a_blank_line_between_two_blocks_gets_none() {
    let mut ed = editor("fn a() {\n    x;\n}\n\nfn b() {\n    y;\n}\n");
    let s = Shot::new(&mut ed);
    assert!(s.guide(1, 0) && s.guide(5, 0));
    assert!(!s.guide(3, 0), "between the functions: no guide");
}

#[test]
fn tabs_are_indentation_too() {
    let mut ed = editor("fn a() {\n\tx;\n\n\t\tz;\n}\n");
    let s = Shot::new(&mut ed);
    assert!(s.guide(1, 0), "a tab is one level");
    assert!(s.guide(2, 0), "also through the blank line");
    assert!(s.guide(3, 0) && s.guide(3, 4), "two tabs, two guides (a tab is 4 columns here)");
    assert_eq!(s.cell(3, 8).ch, 'z');
}

#[test]
fn the_block_the_cursor_is_in_has_a_brighter_guide() {
    let mut ed = editor(CODE);
    keys(&mut ed, "4G"); // `    if y {`
    let s = Shot::new(&mut ed);
    // the body of the function: column 0, from `x;` to the closing line of the `if`
    for l in 1..=5 {
        assert_eq!(s.cell(l, 0).style.fg, SCOPE, "line {}", l);
    }
    assert_eq!(s.cell(4, 4).style.fg, GUIDE, "the inner guide is not the cursor's block");
    keys(&mut ed, "j"); // `        z;`
    let s = Shot::new(&mut ed);
    assert_eq!(s.cell(4, 4).style.fg, SCOPE, "now it is the inner block");
    assert_eq!(s.cell(4, 0).style.fg, GUIDE);
    assert_eq!(s.cell(1, 0).style.fg, GUIDE);
    // at the top level there is no block
    keys(&mut ed, "gg");
    let s = Shot::new(&mut ed);
    assert_eq!(s.cell(1, 0).style.fg, GUIDE);
}

#[test]
fn guides_keep_the_selection_background_and_never_cover_text() {
    let mut ed = editor(CODE);
    keys(&mut ed, "jVjj\x1b");
    keys(&mut ed, "2GVjj");
    let s = Shot::new(&mut ed);
    let c = s.cell(2, 0);
    assert_eq!(c.ch, '\u{2502}');
    assert_eq!(c.style.bg, ved::render::BG_SEL, "the selection shows through the guide");
    // a file that has the character itself in its text
    let mut ed = editor("    \u{2502} x\n    y\n");
    let s = Shot::new(&mut ed);
    assert_eq!(s.cell(0, 4).ch, '\u{2502}');
    assert_ne!(s.cell(0, 4).style.fg, GUIDE, "text is drawn as text");
}

#[test]
fn the_option_turns_them_off_and_the_character_can_be_chosen() {
    let mut ed = editor(CODE);
    keys(&mut ed, ":set noindentguides\r");
    let s = Shot::new(&mut ed);
    assert!(!s.guide(1, 0) && !s.guide(4, 4));
    keys(&mut ed, ":set indentguides\r");
    keys(&mut ed, ":set indentchar=|\r");
    let s = Shot::new(&mut ed);
    assert_eq!(s.cell(1, 0).ch, '|');
    // not something that moves the cursor, takes two cells, or is nothing
    for bad in ["", "ab", "\u{65e5}", " "] {
        keys(&mut ed, &format!(":set indentchar={}\r", bad));
        let s = Shot::new(&mut ed);
        assert_eq!(s.cell(1, 0).ch, '|', "{:?} was refused", bad);
    }
    assert!(ed.opts.indent_char == '|');
}

#[test]
fn the_unit_follows_the_files_own_indentation() {
    // two spaces per level in the file (as a file opened from disk would have detected), 4 in the options
    let mut ed = editor("a:\n  b:\n    c: 1\n    d: 2\n");
    ed.docs[0].buf.indent = Some(ved::buffer::Indent { use_tabs: false, width: 2 });
    let s = Shot::new(&mut ed);
    assert!(s.guide(1, 0), "{:?}", s.f.text());
    assert!(s.guide(2, 0) && s.guide(2, 2), "two levels of two columns");
    assert_eq!(s.cell(2, 4).ch, 'c');
}

#[test]
fn wide_and_scrolled_text_keep_their_guides_in_place() {
    // deep indentation, a long line, and the window scrolled to the right
    let long = format!("{}x{}", " ".repeat(20), "y".repeat(200));
    let mut ed = editor(&format!("a\n{}\n\n{}\n", long, long));
    ed.docs[0].buf.indent = Some(ved::buffer::Indent { use_tabs: false, width: 4 });
    keys(&mut ed, "2G$");
    let s = Shot::new(&mut ed);
    let left = ed.doc().left;
    assert!(left > 20, "the window scrolled sideways ({})", left);
    // nothing of the guides is left on screen: they are all left of the window
    assert!(!(0..80).any(|c| s.f.get(s.bar + 1, c).ch == '\u{2502}' && c >= s.gw));
    keys(&mut ed, "0");
    let s = Shot::new(&mut ed);
    assert_eq!(ed.doc().left, 0);
    for g in [0, 4, 8, 12, 16] {
        assert!(s.guide(1, g) && s.guide(3, g), "column {}", g);
        assert!(s.guide(2, g), "also on the blank line, column {}", g);
    }
    assert!(!s.guide(1, 20));
}
