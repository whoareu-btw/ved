//! Indent guides: thin vertical lines in the leading whitespace, one per indent level, that carry on
//! through blank lines (the indent-blankline plugin of Neovim) and mark the block the cursor is in.
//!
//! Pure functions on lines of text; `render.rs` draws what they work out.

/// How far (in lines) the search for the next non-blank line goes. A blank line in a very long run of
/// blank lines simply gets no guides from lines beyond that.
pub const REACH: usize = 500;

/// The indentation of `line` in display columns (a tab goes to the next multiple of `ts`), or `None`
/// for a blank line (empty, or nothing but spaces and tabs).
pub fn indent_of(line: &[char], ts: usize) -> Option<usize> {
    let ts = ts.max(1);
    let mut col = 0usize;
    for &c in line {
        match c {
            ' ' => col += 1,
            '\t' => col += ts - col % ts,
            _ => return Some(col),
        }
    }
    None
}

/// The indentation the guides of rows `top..top + n` are drawn for. A non-blank line has its own; a
/// blank line takes the larger of the nearest non-blank lines above and below it, so a guide does not
/// stop for an empty line in the middle of a block.
pub fn row_indents(lines: &[Vec<char>], top: usize, n: usize, ts: usize) -> Vec<usize> {
    let n = n.min(lines.len().saturating_sub(top));
    let own: Vec<Option<usize>> = lines[top.min(lines.len())..top.min(lines.len()) + n].iter().map(|l| indent_of(l, ts)).collect();
    // the nearest non-blank line above the window / below it
    let mut prev = (0..top.min(lines.len())).rev().take(REACH).find_map(|r| indent_of(&lines[r], ts)).unwrap_or(0);
    let mut next = (top + n..lines.len()).take(REACH).find_map(|r| indent_of(&lines[r], ts)).unwrap_or(0);
    let mut above = vec![0; n];
    for (i, o) in own.iter().enumerate() {
        if let Some(x) = o {
            prev = *x;
        }
        above[i] = prev;
    }
    let mut below = vec![0; n];
    for (i, o) in own.iter().enumerate().rev() {
        if let Some(x) = o {
            next = *x;
        }
        below[i] = next;
    }
    (0..n).map(|i| own[i].unwrap_or_else(|| above[i].max(below[i]))).collect()
}

/// The columns that get a guide on a line indented `indent` columns: 0, `unit`, 2 `unit` ... below the
/// indentation, and inside `from..to` (what is on screen).
pub fn guide_cols(indent: usize, unit: usize, from: usize, to: usize) -> impl Iterator<Item = usize> {
    let unit = unit.max(1);
    let end = indent.min(to);
    let first = from.div_ceil(unit).saturating_mul(unit);
    (first..end).step_by(unit)
}

/// The block the cursor row (index `at` into `inds`) is in: the guide column that bounds it and the
/// first and last row (indexes into `inds`) of the block. `None` at the top level.
pub fn scope(inds: &[usize], at: usize, unit: usize) -> Option<(usize, usize, usize)> {
    let unit = unit.max(1);
    let here = *inds.get(at)?;
    if here == 0 {
        return None;
    }
    let col = (here - 1) / unit * unit;
    let mut a = at;
    while a > 0 && inds[a - 1] > col {
        a -= 1;
    }
    let mut b = at;
    while b + 1 < inds.len() && inds[b + 1] > col {
        b += 1;
    }
    Some((col, a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<Vec<char>> {
        s.split('\n').map(|l| l.chars().collect()).collect()
    }

    #[test]
    fn indent_counts_columns_and_knows_blank_lines() {
        let c = |s: &str| s.chars().collect::<Vec<_>>();
        assert_eq!(indent_of(&c("    x"), 4), Some(4));
        assert_eq!(indent_of(&c("\tx"), 4), Some(4));
        assert_eq!(indent_of(&c("  \tx"), 4), Some(4), "a tab goes to the next stop");
        assert_eq!(indent_of(&c(" \t \tx"), 8), Some(16));
        assert_eq!(indent_of(&c(""), 4), None);
        assert_eq!(indent_of(&c("   \t "), 4), None, "whitespace only is blank");
        assert_eq!(indent_of(&c("x"), 0), Some(0), "a tabstop of 0 does not divide by zero");
        assert_eq!(indent_of(&c("\tx"), 0), Some(1));
    }

    #[test]
    fn blank_lines_take_the_larger_neighbour() {
        let l = lines("fn a() {\n    x;\n\n    y;\n}\n\nfn b() {\n\n    z;\n}\n");
        let n = l.len();
        let inds = row_indents(&l, 0, n, 4);
        //            fn a   x;  blank  y;  }   blank  fn b  blank  z;   }  (empty last)
        assert_eq!(inds, vec![0, 4, 4, 4, 0, 0, 0, 4, 4, 0, 0]);
    }

    #[test]
    fn a_window_in_the_middle_looks_beyond_its_edges() {
        let l = lines("    a;\n\n\n\n    b;\n\n\n\n\n");
        // rows 2..4 are blank, their neighbours (rows 0 and 4) are outside the window
        assert_eq!(row_indents(&l, 2, 2, 4), vec![4, 4]);
        // rows 6..8: blank, `b` above (4), nothing below (the lines end): 4
        assert_eq!(row_indents(&l, 6, 2, 4), vec![4, 4]);
        // asking for more rows than there are, or a window past the end
        assert_eq!(row_indents(&l, 8, 50, 4).len(), 2);
        assert!(row_indents(&l, 100, 5, 4).is_empty());
        assert!(row_indents(&[], 0, 5, 4).is_empty());
    }

    #[test]
    fn a_huge_run_of_blank_lines_is_cheap_and_bounded() {
        let mut l = vec![vec!['x']];
        l.extend(std::iter::repeat_n(Vec::new(), 100_000));
        l.push("    y".chars().collect());
        // the window sits in the middle of the run: neither end is within reach, so no guides
        assert_eq!(row_indents(&l, 50_000, 3, 4), vec![0, 0, 0]);
        // near the end of the run the line below is within reach
        assert_eq!(row_indents(&l, 100_000 - 2, 2, 4), vec![4, 4]);
    }

    #[test]
    fn guide_columns_stop_below_the_indent_and_stay_on_screen() {
        assert_eq!(guide_cols(12, 4, 0, 100).collect::<Vec<_>>(), vec![0, 4, 8]);
        assert_eq!(guide_cols(4, 4, 0, 100).collect::<Vec<_>>(), vec![0], "the column of the text itself has none");
        assert_eq!(guide_cols(0, 4, 0, 100).count(), 0);
        // scrolled sideways: only what is visible
        assert_eq!(guide_cols(20, 4, 5, 14).collect::<Vec<_>>(), vec![8, 12]);
        assert_eq!(guide_cols(20, 4, 8, 12).collect::<Vec<_>>(), vec![8]);
        // absurd values neither loop for ever nor overflow
        assert_eq!(guide_cols(usize::MAX, 0, usize::MAX - 3, usize::MAX).count(), 3);
        assert_eq!(guide_cols(5, 4, usize::MAX, usize::MAX).count(), 0);
        assert!(guide_cols(1_000_000, 2, 0, 10).count() == 5);
    }

    #[test]
    fn the_scope_is_the_block_around_the_cursor() {
        let l = lines("fn a() {\n    if x {\n        y;\n\n        z;\n    }\n    w;\n}");
        let inds = row_indents(&l, 0, l.len(), 4);
        assert_eq!(inds, vec![0, 4, 8, 8, 8, 4, 4, 0]);
        // on `y;` the block is the body of the `if`: guide column 4, rows 2..=4 (the blank line is in it)
        assert_eq!(scope(&inds, 2, 4), Some((4, 2, 4)));
        // on `w;` it is the body of the function: column 0, rows 1..=6
        assert_eq!(scope(&inds, 6, 4), Some((0, 1, 6)));
        // on the `if` line itself too
        assert_eq!(scope(&inds, 1, 4), Some((0, 1, 6)));
        assert_eq!(scope(&inds, 0, 4), None);
        assert_eq!(scope(&inds, 7, 4), None);
        assert_eq!(scope(&inds, 99, 4), None);
        assert_eq!(scope(&[], 0, 4), None);
    }
}
