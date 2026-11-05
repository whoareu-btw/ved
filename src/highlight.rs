//! Tiny line-based syntax highlighter. Not a parser: it recognises comments, strings,
//! numbers, keywords, types, macros and a few language specifics, which is enough to make
//! code readable without any dependencies. Multi-line comments/strings are tracked with a
//! per-line start state that is cached per buffer version.

use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Rust,
    Python,
    JsTs,
    C,
    Go,
    Shell,
    Json,
    Toml,
    Markdown,
    Yaml,
    Html,
    Lua,
    Java,
}

pub const NORMAL: u8 = 0;
pub const KEYWORD: u8 = 1;
pub const STRING: u8 = 2;
pub const COMMENT: u8 = 3;
pub const NUMBER: u8 = 4;
pub const TYPE: u8 = 5;
pub const FUNC: u8 = 6;
pub const MACRO: u8 = 7;
pub const ATTR: u8 = 8;
pub const HEADING: u8 = 9;

pub fn lang_for(path: Option<&Path>) -> Option<Lang> {
    let p = path?;
    let name = p.file_name()?.to_str()?;
    match name {
        "Makefile" | "makefile" | "Dockerfile" => return Some(Lang::Shell),
        "Cargo.lock" => return Some(Lang::Toml),
        _ => {}
    }
    Some(match p.extension()?.to_str()? {
        "rs" => Lang::Rust,
        "py" | "pyi" => Lang::Python,
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" => Lang::JsTs,
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "cs" => Lang::C,
        "java" | "kt" | "kts" | "scala" | "swift" => Lang::Java,
        "go" => Lang::Go,
        "sh" | "bash" | "zsh" | "fish" | "mk" => Lang::Shell,
        "json" | "jsonc" => Lang::Json,
        "toml" | "ini" | "cfg" | "conf" => Lang::Toml,
        "md" | "markdown" => Lang::Markdown,
        "yml" | "yaml" => Lang::Yaml,
        "html" | "htm" | "xml" | "svg" => Lang::Html,
        "lua" => Lang::Lua,
        _ => return None,
    })
}

fn keywords(l: Lang) -> &'static [&'static str] {
    match l {
        Lang::Rust => &[
            "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in",
            "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
            "unsafe", "use", "where", "while",
        ],
        Lang::Python => &[
            "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "finally", "for", "from",
            "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield", "True",
            "False", "None", "self",
        ],
        Lang::JsTs => &[
            "async", "await", "break", "case", "catch", "class", "const", "continue", "default", "delete", "do", "else", "enum", "export", "extends",
            "false", "finally", "for", "from", "function", "if", "implements", "import", "in", "instanceof", "interface", "let", "new", "null", "of",
            "private", "protected", "public", "readonly", "return", "static", "super", "switch", "this", "throw", "true", "try", "type", "typeof",
            "undefined", "var", "void", "while", "yield",
        ],
        Lang::C => &[
            "auto", "break", "case", "char", "class", "const", "continue", "default", "do", "double", "else", "enum", "extern", "false", "float", "for",
            "goto", "if", "inline", "int", "long", "namespace", "new", "nullptr", "private", "protected", "public", "register", "return", "short",
            "signed", "sizeof", "static", "struct", "switch", "template", "this", "true", "typedef", "typename", "union", "unsigned", "using", "virtual",
            "void", "volatile", "while", "bool",
        ],
        Lang::Java => &[
            "abstract", "break", "case", "catch", "class", "const", "continue", "default", "do", "else", "enum", "extends", "final", "finally", "for",
            "fun", "if", "implements", "import", "instanceof", "interface", "new", "null", "object", "override", "package", "private", "protected",
            "public", "return", "static", "super", "switch", "this", "throw", "true", "false", "try", "val", "var", "void", "when", "while", "func",
            "let", "guard", "struct",
        ],
        Lang::Go => &[
            "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "for", "func", "go", "goto", "if", "import",
            "interface", "map", "package", "range", "return", "select", "struct", "switch", "type", "var", "true", "false", "nil",
        ],
        Lang::Shell => &[
            "if", "then", "else", "elif", "fi", "for", "while", "do", "done", "case", "esac", "function", "in", "return", "exit", "export", "local",
            "echo", "cd", "set", "unset", "source", "readonly", "shift", "test",
        ],
        Lang::Lua => &[
            "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local", "nil", "not", "or", "repeat", "return",
            "then", "true", "until", "while",
        ],
        Lang::Json | Lang::Toml | Lang::Yaml => &["true", "false", "null"],
        _ => &[],
    }
}

fn type_words(l: Lang) -> &'static [&'static str] {
    match l {
        Lang::Rust => &[
            "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64", "bool", "char", "str", "String", "Vec",
            "Option", "Result", "Box", "Some", "None", "Ok", "Err",
        ],
        Lang::Go => &["int", "int8", "int16", "int32", "int64", "uint", "uint8", "uint16", "uint32", "uint64", "string", "bool", "byte", "rune", "error", "float32", "float64"],
        Lang::Python => &["int", "str", "float", "bool", "list", "dict", "set", "tuple", "bytes", "print", "len", "range"],
        Lang::JsTs => &["string", "number", "boolean", "any", "unknown", "never", "Array", "Promise", "Map", "Set", "console"],
        _ => &[],
    }
}

fn line_comment(l: Lang) -> Option<&'static str> {
    match l {
        Lang::Rust | Lang::JsTs | Lang::C | Lang::Go | Lang::Java => Some("//"),
        Lang::Python | Lang::Shell | Lang::Toml | Lang::Yaml => Some("#"),
        Lang::Lua => Some("--"),
        _ => None,
    }
}

fn has_block_comments(l: Lang) -> bool {
    matches!(l, Lang::Rust | Lang::JsTs | Lang::C | Lang::Go | Lang::Java | Lang::Html)
}

// per-line start states
pub const S_NORMAL: u8 = 0;
pub const S_BLOCK: u8 = 1;
pub const S_TRIPLE_D: u8 = 2;
pub const S_TRIPLE_S: u8 = 3;
pub const S_STR: u8 = 4; // multi-line "..." (Rust)
pub const S_BACKTICK: u8 = 5;
pub const S_FENCE: u8 = 6; // markdown code fence

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}
fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn starts(line: &[char], i: usize, pat: &str) -> bool {
    for (k, p) in (i..).zip(pat.chars()) {
        if k >= line.len() || line[k] != p {
            return false;
        }
    }
    true
}

/// Highlight one line. `state` is the state at the start of the line and is updated to the
/// state at its end. `out` receives one class byte per char.
pub fn highlight_line(lang: Lang, line: &[char], state: &mut u8, out: &mut Vec<u8>) {
    out.clear();
    out.resize(line.len(), NORMAL);
    let n = line.len();
    let mut i = 0;
    if lang == Lang::Markdown {
        markdown_line(line, state, out);
        return;
    }
    if lang == Lang::Html {
        html_line(line, state, out);
        return;
    }
    let lc = line_comment(lang);
    let block = has_block_comments(lang);
    let kws = keywords(lang);
    let tys = type_words(lang);

    // resume a construct that started on an earlier line
    match *state {
        S_BLOCK => {
            while i < n {
                if starts(line, i, "*/") {
                    for x in out.iter_mut().take(i + 2) {
                        *x = COMMENT;
                    }
                    i += 2;
                    *state = S_NORMAL;
                    break;
                }
                i += 1;
            }
            if *state == S_BLOCK {
                out.iter_mut().for_each(|x| *x = COMMENT);
                return;
            }
        }
        S_TRIPLE_D | S_TRIPLE_S => {
            let q = if *state == S_TRIPLE_D { "\"\"\"" } else { "'''" };
            let mut closed = false;
            while i < n {
                if starts(line, i, q) {
                    i += 3;
                    closed = true;
                    break;
                }
                i += 1;
            }
            for x in out.iter_mut().take(i) {
                *x = STRING;
            }
            if !closed {
                return;
            }
            *state = S_NORMAL;
        }
        S_STR | S_BACKTICK => {
            let q = if *state == S_STR { '"' } else { '`' };
            let mut closed = false;
            while i < n {
                if line[i] == '\\' && q == '"' {
                    i += 2;
                    continue;
                }
                if line[i] == q {
                    i += 1;
                    closed = true;
                    break;
                }
                i += 1;
            }
            let end = i.min(n);
            for x in out.iter_mut().take(end) {
                *x = STRING;
            }
            if !closed {
                return;
            }
            *state = S_NORMAL;
        }
        _ => {}
    }
    *state = S_NORMAL;

    if lang == Lang::Rust && starts(line, i, "#[") || lang == Lang::Rust && starts(line, i, "#![") {
        let mut j = i;
        while j < n && line[j] != ']' {
            j += 1;
        }
        let end = (j + 1).min(n);
        for x in out.iter_mut().take(end).skip(i) {
            *x = ATTR;
        }
        i = end;
    }
    if lang == Lang::C && line.iter().find(|c| !c.is_whitespace()) == Some(&'#') {
        let k = line.iter().position(|c| *c == '#').unwrap();
        let mut j = k + 1;
        while j < n && (line[j].is_alphabetic() || line[j].is_whitespace() && j == k + 1) {
            j += 1;
        }
        for x in out.iter_mut().take(j).skip(k) {
            *x = MACRO;
        }
        i = i.max(j);
    }
    if lang == Lang::Toml && line.iter().find(|c| !c.is_whitespace()) == Some(&'[') {
        for x in out.iter_mut() {
            *x = TYPE;
        }
        // comment tails still handled below
    }
    if lang == Lang::Yaml || lang == Lang::Toml || lang == Lang::Json {
        // keys: text before the first ':' / '=' on the line
        let mut j = i;
        while j < n && line[j].is_whitespace() {
            j += 1;
        }
        if j < n && line[j] == '-' && lang == Lang::Yaml {
            j += 1;
            while j < n && line[j].is_whitespace() {
                j += 1;
            }
        }
        let key_start = j;
        let mut quote: Option<char> = None;
        while j < n {
            let c = line[j];
            match quote {
                Some(q) if c == q => quote = None,
                Some(_) => {}
                None if c == '"' || c == '\'' => quote = Some(c),
                None if c == ':' && (lang != Lang::Yaml || j + 1 >= n || line[j + 1] == ' ') => break,
                None if c == '=' && lang == Lang::Toml => break,
                None if c == '#' && lang != Lang::Json => {
                    j = n;
                    break;
                }
                None => {}
            }
            j += 1;
        }
        if j < n && out.get(key_start) == Some(&NORMAL) {
            for x in out.iter_mut().take(j).skip(key_start) {
                *x = FUNC;
            }
        }
    }

    while i < n {
        let c = line[i];
        // line comment
        if let Some(lcs) = lc {
            if starts(line, i, lcs) && !(lang == Lang::Shell && i > 0 && (line[i - 1] == '$' || line[i - 1] == '{')) {
                if lang == Lang::Python || lang == Lang::Shell || lang == Lang::Toml || lang == Lang::Yaml {
                    if i == 0 || !is_ident(line[i - 1]) {
                        for x in out.iter_mut().skip(i) {
                            *x = COMMENT;
                        }
                        return;
                    }
                } else {
                    for x in out.iter_mut().skip(i) {
                        *x = COMMENT;
                    }
                    return;
                }
            }
        }
        if block && starts(line, i, "/*") {
            let st = i;
            i += 2;
            let mut closed = false;
            while i < n {
                if starts(line, i, "*/") {
                    i += 2;
                    closed = true;
                    break;
                }
                i += 1;
            }
            for x in out.iter_mut().take(i.min(n)).skip(st) {
                *x = COMMENT;
            }
            if !closed {
                *state = S_BLOCK;
                return;
            }
            continue;
        }
        // strings
        if c == '"' || c == '\'' || (c == '`' && matches!(lang, Lang::JsTs | Lang::Go | Lang::Shell)) {
            if lang == Lang::Rust && c == '\'' {
                // lifetime or char literal?
                let is_char = (i + 2 < n && line[i + 2] == '\'') || (i + 3 < n && line[i + 1] == '\\' && line[i + 3] == '\'');
                if !is_char {
                    let st = i;
                    i += 1;
                    while i < n && is_ident(line[i]) {
                        i += 1;
                    }
                    for x in out.iter_mut().take(i).skip(st) {
                        *x = TYPE;
                    }
                    continue;
                }
            }
            if lang == Lang::Python && (starts(line, i, "\"\"\"") || starts(line, i, "'''")) {
                let q = if c == '"' { "\"\"\"" } else { "'''" };
                let st = i;
                i += 3;
                let mut closed = false;
                while i < n {
                    if starts(line, i, q) {
                        i += 3;
                        closed = true;
                        break;
                    }
                    i += 1;
                }
                for x in out.iter_mut().take(i.min(n)).skip(st) {
                    *x = STRING;
                }
                if !closed {
                    *state = if c == '"' { S_TRIPLE_D } else { S_TRIPLE_S };
                    return;
                }
                continue;
            }
            let st = i;
            i += 1;
            let mut closed = false;
            while i < n {
                if line[i] == '\\' && c != '`' {
                    i += 2;
                    continue;
                }
                if line[i] == c {
                    i += 1;
                    closed = true;
                    break;
                }
                i += 1;
            }
            for x in out.iter_mut().take(i.min(n)).skip(st) {
                *x = STRING;
            }
            if !closed
                && ((lang == Lang::Rust && c == '"') || c == '`') {
                    *state = if c == '"' { S_STR } else { S_BACKTICK };
                    return;
                }
            continue;
        }
        // numbers
        if c.is_ascii_digit() && (i == 0 || !is_ident(line[i - 1])) {
            let st = i;
            i += 1;
            while i < n && (line[i].is_ascii_alphanumeric() || line[i] == '_' || (line[i] == '.' && i + 1 < n && line[i + 1].is_ascii_digit())) {
                i += 1;
            }
            for x in out.iter_mut().take(i).skip(st) {
                *x = NUMBER;
            }
            continue;
        }
        // shell variables
        if lang == Lang::Shell && c == '$' {
            let st = i;
            i += 1;
            if i < n && line[i] == '{' {
                while i < n && line[i] != '}' {
                    i += 1;
                }
                i = (i + 1).min(n);
            } else {
                while i < n && is_ident(line[i]) {
                    i += 1;
                }
            }
            for x in out.iter_mut().take(i).skip(st) {
                *x = TYPE;
            }
            continue;
        }
        // identifiers
        if is_ident_start(c) {
            let st = i;
            while i < n && is_ident(line[i]) {
                i += 1;
            }
            let w: String = line[st..i].iter().collect();
            let mut class = NORMAL;
            if kws.contains(&w.as_str()) {
                class = KEYWORD;
            } else if tys.contains(&w.as_str()) {
                class = TYPE;
            } else if lang == Lang::Rust && i < n && line[i] == '!' && !(i + 1 < n && line[i + 1] == '=') {
                class = MACRO;
                i += 1;
            } else if i < n && line[i] == '(' {
                class = FUNC;
            } else if c.is_uppercase() && w.len() > 1 && !w.chars().all(|ch| ch.is_uppercase() || ch == '_' || ch.is_ascii_digit()) {
                class = TYPE;
            } else if c.is_uppercase() && w.len() > 1 && lang != Lang::Shell {
                class = NUMBER; // CONSTANTS
            }
            if class != NORMAL {
                for x in out.iter_mut().take(i).skip(st) {
                    *x = class;
                }
            }
            continue;
        }
        i += 1;
    }
}

fn markdown_line(line: &[char], state: &mut u8, out: &mut [u8]) {
    let n = line.len();
    let trimmed: String = line.iter().collect::<String>().trim_start().to_string();
    if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
        *state = if *state == S_FENCE { S_NORMAL } else { S_FENCE };
        out.iter_mut().for_each(|x| *x = COMMENT);
        return;
    }
    if *state == S_FENCE {
        out.iter_mut().for_each(|x| *x = STRING);
        return;
    }
    if trimmed.starts_with('#') {
        out.iter_mut().for_each(|x| *x = HEADING);
        return;
    }
    if trimmed.starts_with('>') {
        out.iter_mut().for_each(|x| *x = COMMENT);
        return;
    }
    let mut i = 0;
    // list markers
    let ws = line.iter().take_while(|c| c.is_whitespace()).count();
    if ws < n && matches!(line[ws], '-' | '*' | '+') && ws + 1 < n && line[ws + 1] == ' ' {
        out[ws] = KEYWORD;
    }
    while i < n {
        if line[i] == '`' {
            let st = i;
            i += 1;
            while i < n && line[i] != '`' {
                i += 1;
            }
            let end = (i + 1).min(n);
            for x in out.iter_mut().take(end).skip(st) {
                *x = STRING;
            }
            i = end;
            continue;
        }
        if line[i] == '[' {
            // [text](url)
            if let Some(close) = line[i..].iter().position(|c| *c == ']') {
                let e = i + close;
                if e + 1 < n && line[e + 1] == '(' {
                    if let Some(pc) = line[e..].iter().position(|c| *c == ')') {
                        for x in out.iter_mut().take(e + pc + 1).skip(i) {
                            *x = FUNC;
                        }
                        i = e + pc + 1;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
}

fn html_line(line: &[char], state: &mut u8, out: &mut [u8]) {
    let n = line.len();
    let mut i = 0;
    if *state == S_BLOCK {
        while i < n && !starts(line, i, "-->") {
            i += 1;
        }
        let end = if i < n { i + 3 } else { n };
        for x in out.iter_mut().take(end) {
            *x = COMMENT;
        }
        if i >= n {
            return;
        }
        i = end;
        *state = S_NORMAL;
    }
    while i < n {
        if starts(line, i, "<!--") {
            let st = i;
            i += 4;
            let mut closed = false;
            while i < n {
                if starts(line, i, "-->") {
                    i += 3;
                    closed = true;
                    break;
                }
                i += 1;
            }
            for x in out.iter_mut().take(i.min(n)).skip(st) {
                *x = COMMENT;
            }
            if !closed {
                *state = S_BLOCK;
                return;
            }
            continue;
        }
        if line[i] == '<' {
            let st = i;
            i += 1;
            while i < n && line[i] != '>' {
                if line[i] == '"' {
                    let s2 = i;
                    i += 1;
                    while i < n && line[i] != '"' {
                        i += 1;
                    }
                    let e2 = (i + 1).min(n);
                    for x in out.iter_mut().take(e2).skip(s2) {
                        *x = STRING;
                    }
                    i = e2;
                    continue;
                }
                out[i] = if i == st + 1 || (i == st + 2 && line[st + 1] == '/') { KEYWORD } else if out[i] == NORMAL { TYPE } else { out[i] };
                i += 1;
            }
            let end = (i + 1).min(n);
            out[st] = KEYWORD;
            if end > 0 && end <= n {
                out[end - 1] = KEYWORD;
            }
            i = end;
            continue;
        }
        i += 1;
    }
}

/// Per-buffer cache of "state at the start of each line".
#[derive(Default)]
pub struct HlCache {
    ver: i32,
    lang: Option<Lang>,
    states: Vec<u8>,
}

impl HlCache {
    /// Make sure start-states are known for rows 0..=upto and return the state for `row`.
    pub fn state_at(&mut self, lang: Lang, version: i32, lines: &[Vec<char>], row: usize) -> u8 {
        if self.ver != version || self.lang != Some(lang) || self.states.is_empty() {
            self.ver = version;
            self.lang = Some(lang);
            self.states.clear();
            self.states.push(S_NORMAL);
        }
        let mut scratch = Vec::new();
        while self.states.len() <= row.min(lines.len()) {
            let r = self.states.len() - 1;
            let mut st = self.states[r];
            if r < lines.len() {
                highlight_line(lang, &lines[r], &mut st, &mut scratch);
            }
            self.states.push(st);
        }
        self.states[row.min(self.states.len() - 1)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hl(lang: Lang, s: &str) -> (Vec<u8>, u8) {
        let line: Vec<char> = s.chars().collect();
        let mut st = 0;
        let mut out = Vec::new();
        highlight_line(lang, &line, &mut st, &mut out);
        (out, st)
    }

    #[test]
    fn rust_basics() {
        let (o, _) = hl(Lang::Rust, "let x = 42; // hi");
        assert_eq!(o[0], KEYWORD);
        assert_eq!(o[8], NUMBER);
        assert_eq!(o[13], COMMENT);
        let (o, _) = hl(Lang::Rust, "println!(\"a\")");
        assert_eq!(o[0], MACRO);
        assert_eq!(o[9], STRING);
    }

    #[test]
    fn block_comment_state() {
        let (o, st) = hl(Lang::Rust, "a /* open");
        assert_eq!(st, S_BLOCK);
        assert_eq!(o[2], COMMENT);
        let line: Vec<char> = "close */ fn".chars().collect();
        let mut st = S_BLOCK;
        let mut out = Vec::new();
        highlight_line(Lang::Rust, &line, &mut st, &mut out);
        assert_eq!(st, S_NORMAL);
        assert_eq!(out[0], COMMENT);
        assert_eq!(out[9], KEYWORD);
    }

    #[test]
    fn python_triple_quotes() {
        let (o, st) = hl(Lang::Python, "x = \"\"\"doc");
        assert_eq!(st, S_TRIPLE_D);
        assert_eq!(o[5], STRING);
    }

    #[test]
    fn rust_lifetime_not_a_string() {
        let (o, st) = hl(Lang::Rust, "fn f<'a>(x: &'a str) -> char { 'x' }");
        assert_eq!(st, S_NORMAL);
        assert_eq!(o[5], TYPE);
        assert_eq!(*o.iter().rev().nth(2).unwrap(), STRING);
    }

    #[test]
    fn cache_states() {
        let lines: Vec<Vec<char>> = ["/* a", "b", "c */", "d"].iter().map(|s| s.chars().collect()).collect();
        let mut c = HlCache::default();
        assert_eq!(c.state_at(Lang::C, 1, &lines, 1), S_BLOCK);
        assert_eq!(c.state_at(Lang::C, 1, &lines, 3), S_NORMAL);
    }
}
