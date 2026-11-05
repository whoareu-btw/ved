//! Pure cursor-motion and text-object logic over `&[Vec<char>]`.
//! Kept free of editor state so it can be unit-tested directly.

use crate::buffer::Pos;

pub type Lines = [Vec<char>];

/// 0 = whitespace, 1 = punctuation, 2 = word char. With `big`, any non-space is 2.
pub fn class(c: char, big: bool) -> u8 {
    if c.is_whitespace() {
        0
    } else if big || is_word_char(c) {
        2
    } else {
        1
    }
}

pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `w` / `W`. May return (last_row, len) when there is no further word.
pub fn word_fwd(l: &Lines, p: Pos, big: bool) -> Pos {
    let (mut r, mut c) = p;
    let n = l.len();
    if c < l[r].len() {
        let k = class(l[r][c], big);
        if k != 0 {
            while c < l[r].len() && class(l[r][c], big) == k {
                c += 1;
            }
        }
    }
    loop {
        if c >= l[r].len() {
            if r + 1 >= n {
                return (r, l[r].len());
            }
            r += 1;
            c = 0;
            if l[r].is_empty() {
                return (r, 0);
            }
            continue;
        }
        if class(l[r][c], big) == 0 {
            c += 1;
        } else {
            return (r, c);
        }
    }
}

/// `e` / `E`
pub fn word_end(l: &Lines, p: Pos, big: bool) -> Pos {
    let (mut r, mut c) = p;
    let last = |r: usize| l[r].len().saturating_sub(1);
    if c + 1 < l[r].len() {
        c += 1;
    } else {
        if r + 1 >= l.len() {
            return (r, last(r));
        }
        r += 1;
        c = 0;
    }
    loop {
        if c >= l[r].len() {
            if r + 1 >= l.len() {
                return (r, last(r));
            }
            r += 1;
            c = 0;
            continue;
        }
        if class(l[r][c], big) == 0 {
            c += 1;
        } else {
            break;
        }
    }
    let k = class(l[r][c], big);
    while c + 1 < l[r].len() && class(l[r][c + 1], big) == k {
        c += 1;
    }
    (r, c)
}

/// `b` / `B`
pub fn word_back(l: &Lines, p: Pos, big: bool) -> Pos {
    let (mut r, mut c) = p;
    if c > 0 {
        c -= 1;
        c = c.min(l[r].len().saturating_sub(1));
    } else if r > 0 {
        r -= 1;
        if l[r].is_empty() {
            return (r, 0);
        }
        c = l[r].len() - 1;
    } else {
        return (0, 0);
    }
    loop {
        if !l[r].is_empty() && class(l[r][c], big) != 0 {
            break;
        }
        if c > 0 {
            c -= 1;
        } else if r > 0 {
            r -= 1;
            if l[r].is_empty() {
                return (r, 0);
            }
            c = l[r].len() - 1;
        } else {
            return (0, 0);
        }
    }
    let k = class(l[r][c], big);
    while c > 0 && class(l[r][c - 1], big) == k {
        c -= 1;
    }
    (r, c)
}

/// `ge` / `gE`
pub fn word_end_back(l: &Lines, p: Pos, big: bool) -> Pos {
    let (mut r, mut c) = p;
    if c < l[r].len() {
        let k = class(l[r][c], big);
        if k != 0 {
            while c > 0 && class(l[r][c - 1], big) == k {
                c -= 1;
            }
        }
    }
    loop {
        if c > 0 {
            c -= 1;
        } else if r > 0 {
            r -= 1;
            if l[r].is_empty() {
                return (r, 0);
            }
            c = l[r].len() - 1;
        } else {
            return (0, 0);
        }
        if c < l[r].len() && class(l[r][c], big) != 0 {
            return (r, c);
        }
    }
}

/// `f` `F` `t` `T`. `from` is the cursor column. Returns the target column.
pub fn find_char(
    line: &[char],
    from: usize,
    ch: char,
    forward: bool,
    till: bool,
    count: usize,
    repeat: bool,
) -> Option<usize> {
    let mut remaining = count.max(1);
    if forward {
        let mut i = from + 1;
        if repeat && till {
            i += 1;
        }
        while i < line.len() {
            if line[i] == ch {
                remaining -= 1;
                if remaining == 0 {
                    return Some(if till { i - 1 } else { i });
                }
            }
            i += 1;
        }
        None
    } else {
        let mut i = from as isize - 1;
        if repeat && till {
            i -= 1;
        }
        while i >= 0 {
            if line[i as usize] == ch {
                remaining -= 1;
                if remaining == 0 {
                    return Some(if till { i as usize + 1 } else { i as usize });
                }
            }
            i -= 1;
        }
        None
    }
}

/// `}`: returns a position (row, col) suitable as an exclusive motion target.
pub fn para_fwd(l: &Lines, r: usize) -> Pos {
    let n = l.len();
    let mut i = r;
    while i < n && l[i].is_empty() {
        i += 1;
    }
    while i < n && !l[i].is_empty() {
        i += 1;
    }
    if i >= n {
        (n - 1, l[n - 1].len())
    } else {
        (i, 0)
    }
}

/// `{`
pub fn para_back(l: &Lines, r: usize) -> Pos {
    let mut i = r as isize;
    while i >= 0 && l[i as usize].is_empty() {
        i -= 1;
    }
    while i >= 0 && !l[i as usize].is_empty() {
        i -= 1;
    }
    (if i < 0 { 0 } else { i as usize }, 0)
}

fn bracket_info(c: char) -> Option<(char, char, bool)> {
    match c {
        '(' => Some(('(', ')', true)),
        ')' => Some(('(', ')', false)),
        '[' => Some(('[', ']', true)),
        ']' => Some(('[', ']', false)),
        '{' => Some(('{', '}', true)),
        '}' => Some(('{', '}', false)),
        _ => None,
    }
}

/// `%`
pub fn match_pair(l: &Lines, p: Pos) -> Option<Pos> {
    let line = &l[p.0];
    let mut c = p.1;
    while c < line.len() && bracket_info(line[c]).is_none() {
        c += 1;
    }
    if c >= line.len() {
        return None;
    }
    let (open, close, fwd) = bracket_info(line[c]).unwrap();
    let mut depth = 0i32;
    if fwd {
        let (mut r, mut i) = (p.0, c);
        loop {
            while i < l[r].len() {
                let ch = l[r][i];
                if ch == open {
                    depth += 1;
                } else if ch == close {
                    depth -= 1;
                    if depth == 0 {
                        return Some((r, i));
                    }
                }
                i += 1;
            }
            r += 1;
            if r >= l.len() {
                return None;
            }
            i = 0;
        }
    } else {
        let (mut r, mut i) = (p.0 as isize, c as isize);
        loop {
            while i >= 0 {
                let ch = l[r as usize][i as usize];
                if ch == close {
                    depth += 1;
                } else if ch == open {
                    depth -= 1;
                    if depth == 0 {
                        return Some((r as usize, i as usize));
                    }
                }
                i -= 1;
            }
            r -= 1;
            if r < 0 {
                return None;
            }
            i = l[r as usize].len() as isize - 1;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjRange {
    pub start: Pos,
    /// Exclusive for charwise ranges; for linewise ranges only `.0` (inclusive row) matters.
    pub end: Pos,
    pub linewise: bool,
}

fn chars_range(start: Pos, end: Pos) -> ObjRange {
    ObjRange { start, end, linewise: false }
}

/// `iw` `aw` `iW` `aW`
pub fn obj_word(l: &Lines, p: Pos, around: bool, big: bool) -> Option<ObjRange> {
    let (r, c) = p;
    let line = &l[r];
    if line.is_empty() {
        return Some(chars_range((r, 0), (r, 0)));
    }
    let c = c.min(line.len() - 1);
    let k = class(line[c], big);
    let mut s = c;
    while s > 0 && class(line[s - 1], big) == k {
        s -= 1;
    }
    let mut e = c + 1;
    while e < line.len() && class(line[e], big) == k {
        e += 1;
    }
    if !around {
        return Some(chars_range((r, s), (r, e)));
    }
    if k != 0 {
        let mut e2 = e;
        while e2 < line.len() && class(line[e2], big) == 0 {
            e2 += 1;
        }
        if e2 > e {
            return Some(chars_range((r, s), (r, e2)));
        }
        let mut s2 = s;
        while s2 > 0 && class(line[s2 - 1], big) == 0 {
            s2 -= 1;
        }
        Some(chars_range((r, s2), (r, e)))
    } else {
        let mut e2 = e;
        if e2 < line.len() {
            let k2 = class(line[e2], big);
            while e2 < line.len() && class(line[e2], big) == k2 {
                e2 += 1;
            }
        }
        Some(chars_range((r, s), (r, e2)))
    }
}

/// `i"` `a"` `i'` `a'` ``i` `` ``a` ``
pub fn obj_quote(l: &Lines, p: Pos, q: char, around: bool) -> Option<ObjRange> {
    let (r, c) = p;
    let line = &l[r];
    let mut idx: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < line.len() {
        if line[i] == '\\' {
            i += 2;
            continue;
        }
        if line[i] == q {
            idx.push(i);
        }
        i += 1;
    }
    let mut pair = None;
    let mut k = 0;
    while k + 1 < idx.len() {
        let (a, b) = (idx[k], idx[k + 1]);
        if a <= c && c <= b {
            pair = Some((a, b));
            break;
        }
        if a > c && pair.is_none() {
            pair = Some((a, b));
            break;
        }
        k += 2;
    }
    let (a, b) = pair?;
    if !around {
        return Some(chars_range((r, a + 1), (r, b)));
    }
    let mut e = b + 1;
    while e < line.len() && line[e].is_whitespace() {
        e += 1;
    }
    if e > b + 1 {
        return Some(chars_range((r, a), (r, e)));
    }
    let mut s = a;
    while s > 0 && line[s - 1].is_whitespace() {
        s -= 1;
    }
    Some(chars_range((r, s), (r, b + 1)))
}

/// `i(` `a(` `ib` `i{` `iB` `i[` etc. `open`/`close` are the pair characters.
pub fn obj_bracket(l: &Lines, p: Pos, open: char, close: char, around: bool) -> Option<ObjRange> {
    // find the enclosing open bracket (the cursor on a bracket counts as inside)
    let (mut r, mut c) = (p.0 as isize, p.1 as isize);
    let cur = l[p.0].get(p.1).copied();
    let mut depth = 0i32;
    let mut start: Option<Pos> = None;
    if cur == Some(open) {
        start = Some(p);
    } else {
        if cur == Some(close) {
            depth = 1; // skip our own closer
        }
        'outer: loop {
            c -= 1;
            while c < 0 {
                r -= 1;
                if r < 0 {
                    break 'outer;
                }
                c = l[r as usize].len() as isize - 1;
            }
            let ch = l[r as usize][c as usize];
            if ch == close {
                depth += 1;
            } else if ch == open {
                if depth == 0 {
                    start = Some((r as usize, c as usize));
                    break;
                }
                depth -= 1;
            }
        }
    }
    let s = start?;
    // find the matching close
    let (mut r, mut c) = (s.0, s.1 + 1);
    let mut depth = 0i32;
    let e;
    'find: loop {
        while c >= l[r].len() {
            r += 1;
            c = 0;
            if r >= l.len() {
                return None;
            }
        }
        let ch = l[r][c];
        if ch == open {
            depth += 1;
        } else if ch == close {
            if depth == 0 {
                e = (r, c);
                break 'find;
            }
            depth -= 1;
        }
        c += 1;
    }
    if around {
        return Some(chars_range(s, (e.0, e.1 + 1)));
    }
    // inner: braces on their own lines make the object linewise
    let open_at_eol = s.1 + 1 >= l[s.0].len();
    let close_only_ws_before = l[e.0][..e.1].iter().all(|ch| ch.is_whitespace());
    if open_at_eol && close_only_ws_before && e.0 > s.0 {
        if e.0 == s.0 + 1 {
            return Some(chars_range((s.0 + 1, 0), (s.0 + 1, 0)));
        }
        return Some(ObjRange { start: (s.0 + 1, 0), end: (e.0 - 1, 0), linewise: true });
    }
    Some(chars_range((s.0, s.1 + 1), e))
}

/// Like `obj_bracket`, but `n > 1` selects the n-th enclosing pair (`d2i(`).
pub fn obj_bracket_n(l: &Lines, p: Pos, open: char, close: char, around: bool, n: usize) -> Option<ObjRange> {
    let mut pos = p;
    for _ in 1..n.max(1) {
        let a = obj_bracket(l, pos, open, close, true)?;
        let s = a.start;
        pos = if s.1 > 0 {
            (s.0, s.1 - 1)
        } else if s.0 > 0 {
            (s.0 - 1, l[s.0 - 1].len().saturating_sub(1))
        } else {
            return None;
        };
    }
    obj_bracket(l, pos, open, close, around)
}

/// `ip` `ap` (linewise)
pub fn obj_paragraph(l: &Lines, r: usize, around: bool) -> Option<ObjRange> {
    let n = l.len();
    let blank = |i: usize| l[i].iter().all(|c| c.is_whitespace());
    let kind = blank(r);
    let mut s = r;
    while s > 0 && blank(s - 1) == kind {
        s -= 1;
    }
    let mut e = r;
    while e + 1 < n && blank(e + 1) == kind {
        e += 1;
    }
    if around {
        if !kind {
            // trailing blank lines
            let mut e2 = e;
            while e2 + 1 < n && blank(e2 + 1) {
                e2 += 1;
            }
            if e2 > e {
                e = e2;
            } else {
                while s > 0 && blank(s - 1) {
                    s -= 1;
                }
            }
        } else if e + 1 < n {
            // blank run + following paragraph
            e += 1;
            while e + 1 < n && !blank(e + 1) {
                e += 1;
            }
        }
    }
    Some(ObjRange { start: (s, 0), end: (e, 0), linewise: true })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<Vec<char>> {
        s.split('\n').map(|l| l.chars().collect()).collect()
    }

    #[test]
    fn w_motions() {
        let l = lines("foo bar.baz\n\nqux");
        assert_eq!(word_fwd(&l, (0, 0), false), (0, 4));
        assert_eq!(word_fwd(&l, (0, 4), false), (0, 7));
        assert_eq!(word_fwd(&l, (0, 7), false), (0, 8));
        assert_eq!(word_fwd(&l, (0, 8), false), (1, 0)); // empty line is a stop
        assert_eq!(word_fwd(&l, (1, 0), false), (2, 0));
        assert_eq!(word_fwd(&l, (2, 0), false), (2, 3));
        assert_eq!(word_fwd(&l, (0, 4), true), (1, 0));
    }

    #[test]
    fn e_and_b_motions() {
        let l = lines("foo bar.baz\nqux");
        assert_eq!(word_end(&l, (0, 0), false), (0, 2));
        assert_eq!(word_end(&l, (0, 2), false), (0, 6));
        assert_eq!(word_end(&l, (0, 6), false), (0, 7));
        assert_eq!(word_end(&l, (0, 10), false), (1, 2));
        assert_eq!(word_back(&l, (0, 8), false), (0, 7));
        assert_eq!(word_back(&l, (0, 7), false), (0, 4));
        assert_eq!(word_back(&l, (0, 4), false), (0, 0));
        assert_eq!(word_back(&l, (1, 0), false), (0, 8));
        assert_eq!(word_back(&l, (0, 0), false), (0, 0));
        assert_eq!(word_end_back(&l, (0, 4), false), (0, 2));
    }

    #[test]
    fn f_t_and_repeat() {
        let line: Vec<char> = "a,b,c,d".chars().collect();
        assert_eq!(find_char(&line, 0, ',', true, false, 1, false), Some(1));
        assert_eq!(find_char(&line, 0, ',', true, false, 2, false), Some(3));
        assert_eq!(find_char(&line, 0, ',', true, true, 1, false), Some(0));
        assert_eq!(find_char(&line, 0, ',', true, true, 1, true), Some(2));
        assert_eq!(find_char(&line, 6, ',', false, false, 1, false), Some(5));
        assert_eq!(find_char(&line, 6, 'z', false, false, 1, false), None);
    }

    #[test]
    fn pairs() {
        let l = lines("fn f(a, (b)) {\n  x\n}");
        assert_eq!(match_pair(&l, (0, 4)), Some((0, 11)));
        assert_eq!(match_pair(&l, (0, 11)), Some((0, 4)));
        assert_eq!(match_pair(&l, (0, 13)), Some((2, 0)));
        assert_eq!(match_pair(&l, (2, 0)), Some((0, 13)));
    }

    #[test]
    fn word_objects() {
        let l = lines("foo  bar baz");
        let r = obj_word(&l, (0, 1), false, false).unwrap();
        assert_eq!((r.start, r.end), ((0, 0), (0, 3)));
        let r = obj_word(&l, (0, 1), true, false).unwrap();
        assert_eq!((r.start, r.end), ((0, 0), (0, 5)));
        let r = obj_word(&l, (0, 10), true, false).unwrap(); // last word: takes leading space
        assert_eq!((r.start, r.end), ((0, 8), (0, 12)));
    }

    #[test]
    fn quote_objects() {
        let l = lines(r#"say("hi there", "bye")"#);
        let r = obj_quote(&l, (0, 7), '"', false).unwrap();
        assert_eq!((r.start, r.end), ((0, 5), (0, 13)));
        let r = obj_quote(&l, (0, 7), '"', true).unwrap();
        assert_eq!((r.start, r.end), ((0, 4), (0, 14))); // comma follows: no trailing space to take
        let r = obj_quote(&l, (0, 0), '"', false).unwrap(); // cursor before: first pair
        assert_eq!((r.start, r.end), ((0, 5), (0, 13)));
        let l = lines(r#"x = "abc"  y"#);
        let r = obj_quote(&l, (0, 6), '"', true).unwrap(); // trailing whitespace is included
        assert_eq!((r.start, r.end), ((0, 4), (0, 11)));
    }

    #[test]
    fn bracket_objects() {
        let l = lines("f(a, (b), c)");
        let r = obj_bracket(&l, (0, 3), '(', ')', false).unwrap();
        assert_eq!((r.start, r.end), ((0, 2), (0, 11)));
        let r = obj_bracket(&l, (0, 6), '(', ')', true).unwrap();
        assert_eq!((r.start, r.end), ((0, 5), (0, 8)));
        let l = lines("fn f() {\n    body;\n}");
        let r = obj_bracket(&l, (1, 2), '{', '}', false).unwrap();
        assert!(r.linewise);
        assert_eq!((r.start.0, r.end.0), (1, 1));
        assert!(obj_bracket(&l, (0, 0), '[', ']', false).is_none());
    }

    #[test]
    fn nested_bracket_counts() {
        let l = lines("a (b (c) d) e");
        let r = obj_bracket_n(&l, (0, 6), '(', ')', false, 2).unwrap();
        assert_eq!((r.start, r.end), ((0, 3), (0, 10)));
        let r = obj_bracket_n(&l, (0, 6), '(', ')', true, 2).unwrap();
        assert_eq!((r.start, r.end), ((0, 2), (0, 11)));
        assert!(obj_bracket_n(&l, (0, 6), '(', ')', false, 3).is_none());
    }

    #[test]
    fn paragraphs() {
        let l = lines("a\nb\n\nc");
        assert_eq!(para_fwd(&l, 0), (2, 0));
        assert_eq!(para_fwd(&l, 2), (4 - 1, 1));
        assert_eq!(para_back(&l, 3), (2, 0));
        let r = obj_paragraph(&l, 0, false).unwrap();
        assert_eq!((r.start.0, r.end.0), (0, 1));
        let r = obj_paragraph(&l, 0, true).unwrap();
        assert_eq!((r.start.0, r.end.0), (0, 2));
    }
}
