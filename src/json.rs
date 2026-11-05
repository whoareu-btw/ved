//! Minimal JSON value, parser and serializer (enough for JSON-RPC / LSP).

use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

static NULL: Json = Json::Null;

impl Json {
    pub fn get(&self, key: &str) -> &Json {
        if let Json::Obj(v) = self {
            for (k, val) in v {
                if k == key {
                    return val;
                }
            }
        }
        &NULL
    }

    pub fn at(&self, i: usize) -> &Json {
        match self {
            Json::Arr(v) => v.get(i).unwrap_or(&NULL),
            _ => &NULL,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        self.as_f64().map(|n| n as i64)
    }

    pub fn as_usize(&self) -> Option<usize> {
        self.as_f64().filter(|n| *n >= 0.0).map(|n| n as usize)
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn arr(&self) -> &[Json] {
        match self {
            Json::Arr(v) => v,
            _ => &[],
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    pub fn parse(s: &str) -> Result<Json, String> {
        let mut p = Parser { b: s.as_bytes(), i: 0 };
        p.ws();
        let v = p.value(0)?;
        p.ws();
        if p.i != p.b.len() {
            return Err(format!("trailing data at byte {}", p.i));
        }
        Ok(v)
    }
}

pub fn obj(pairs: Vec<(&str, Json)>) -> Json {
    Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

impl From<&str> for Json {
    fn from(s: &str) -> Json {
        Json::Str(s.to_string())
    }
}
impl From<String> for Json {
    fn from(s: String) -> Json {
        Json::Str(s)
    }
}
impl From<bool> for Json {
    fn from(b: bool) -> Json {
        Json::Bool(b)
    }
}
impl From<i64> for Json {
    fn from(n: i64) -> Json {
        Json::Num(n as f64)
    }
}
impl From<i32> for Json {
    fn from(n: i32) -> Json {
        Json::Num(n as f64)
    }
}
impl From<usize> for Json {
    fn from(n: usize) -> Json {
        Json::Num(n as f64)
    }
}
impl From<Vec<Json>> for Json {
    fn from(v: Vec<Json>) -> Json {
        Json::Arr(v)
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn eat(&mut self, lit: &str) -> bool {
        if self.b[self.i..].starts_with(lit.as_bytes()) {
            self.i += lit.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > 512 {
            return Err("nesting too deep".into());
        }
        if self.i >= self.b.len() {
            return Err("unexpected end".into());
        }
        match self.b[self.i] {
            b'{' => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.i < self.b.len() && self.b[self.i] == b'}' {
                    self.i += 1;
                    return Ok(Json::Obj(v));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.ws();
                    if !self.eat(":") {
                        return Err(format!("expected ':' at byte {}", self.i));
                    }
                    self.ws();
                    let val = self.value(depth + 1)?;
                    v.push((k, val));
                    self.ws();
                    if self.eat(",") {
                        continue;
                    }
                    if self.eat("}") {
                        return Ok(Json::Obj(v));
                    }
                    return Err(format!("expected ',' or '}}' at byte {}", self.i));
                }
            }
            b'[' => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.i < self.b.len() && self.b[self.i] == b']' {
                    self.i += 1;
                    return Ok(Json::Arr(v));
                }
                loop {
                    self.ws();
                    v.push(self.value(depth + 1)?);
                    self.ws();
                    if self.eat(",") {
                        continue;
                    }
                    if self.eat("]") {
                        return Ok(Json::Arr(v));
                    }
                    return Err(format!("expected ',' or ']' at byte {}", self.i));
                }
            }
            b'"' => Ok(Json::Str(self.string()?)),
            b't' | b'f' | b'n' => {
                if self.eat("true") {
                    Ok(Json::Bool(true))
                } else if self.eat("false") {
                    Ok(Json::Bool(false))
                } else if self.eat("null") {
                    Ok(Json::Null)
                } else {
                    Err(format!("bad literal at byte {}", self.i))
                }
            }
            b'-' | b'0'..=b'9' => {
                let st = self.i;
                while self.i < self.b.len()
                    && matches!(self.b[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    self.i += 1;
                }
                let s = std::str::from_utf8(&self.b[st..self.i]).map_err(|e| e.to_string())?;
                match s.parse::<f64>() {
                    // `1e999` parses to infinity, which JSON cannot represent (and which we could
                    // not print back): treat it as the malformed input it is
                    Ok(n) if n.is_finite() => Ok(Json::Num(n)),
                    _ => Err(format!("bad number '{}'", s)),
                }
            }
            c => Err(format!("unexpected '{}' at byte {}", c as char, self.i)),
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        if self.i + 4 > self.b.len() {
            return Err("short \\u escape".into());
        }
        let digits = &self.b[self.i..self.i + 4];
        // `from_str_radix` would also take a leading '+'
        if !digits.iter().all(|d| d.is_ascii_hexdigit()) {
            return Err("bad \\u escape".into());
        }
        let s = std::str::from_utf8(digits).map_err(|e| e.to_string())?;
        let n = u32::from_str_radix(s, 16).map_err(|_| "bad \\u escape".to_string())?;
        self.i += 4;
        Ok(n)
    }

    fn string(&mut self) -> Result<String, String> {
        if self.i >= self.b.len() || self.b[self.i] != b'"' {
            return Err(format!("expected string at byte {}", self.i));
        }
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            if self.i >= self.b.len() {
                return Err("unterminated string".into());
            }
            let c = self.b[self.i];
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    if self.i >= self.b.len() {
                        return Err("bad escape".into());
                    }
                    let e = self.b[self.i];
                    self.i += 1;
                    match e {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) && self.b[self.i..].starts_with(b"\\u") {
                                // a high surrogate: only a following low surrogate joins it, any
                                // other escape is left to be read on its own
                                let keep = self.i;
                                self.i += 2;
                                match self.hex4() {
                                    Ok(lo) if (0xDC00..0xE000).contains(&lo) => {
                                        cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                    }
                                    _ => self.i = keep,
                                }
                            }
                            let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => return Err("bad escape".into()),
                    }
                }
                _ => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|e| e.to_string())
    }
}

fn write_str(f: &mut fmt::Formatter<'_>, s: &str) -> fmt::Result {
    f.write_str("\"")?;
    for c in s.chars() {
        match c {
            '"' => f.write_str("\\\"")?,
            '\\' => f.write_str("\\\\")?,
            '\n' => f.write_str("\\n")?,
            '\r' => f.write_str("\\r")?,
            '\t' => f.write_str("\\t")?,
            c if (c as u32) < 0x20 => write!(f, "\\u{:04x}", c as u32)?,
            c => write!(f, "{}", c)?,
        }
    }
    f.write_str("\"")
}

impl fmt::Display for Json {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Json::Null => f.write_str("null"),
            Json::Bool(b) => write!(f, "{}", b),
            Json::Num(n) => {
                if !n.is_finite() {
                    f.write_str("null")
                } else if n.fract() == 0.0 && n.abs() < 1e15 {
                    write!(f, "{}", *n as i64)
                } else {
                    write!(f, "{}", n)
                }
            }
            Json::Str(s) => write_str(f, s),
            Json::Arr(v) => {
                f.write_str("[")?;
                for (i, x) in v.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{}", x)?;
                }
                f.write_str("]")
            }
            Json::Obj(v) => {
                f.write_str("{")?;
                for (i, (k, x)) in v.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write_str(f, k)?;
                    f.write_str(":")?;
                    write!(f, "{}", x)?;
                }
                f.write_str("}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_get() {
        let j = Json::parse(r#"{"a":1,"b":[true,null,"x\né😀"],"c":{"d":-2.5e1}}"#).unwrap();
        assert_eq!(j.get("a").as_i64(), Some(1));
        assert_eq!(j.get("b").at(0).as_bool(), Some(true));
        assert!(j.get("b").at(1).is_null());
        assert_eq!(j.get("b").at(2).as_str(), Some("x\né😀"));
        assert_eq!(j.get("c").get("d").as_f64(), Some(-25.0));
        assert!(j.get("missing").is_null());
    }

    #[test]
    fn roundtrip() {
        let j = obj(vec![
            ("id", 7.into()),
            ("s", "a\"b\\c\n\t\u{1}".into()),
            ("arr", vec![Json::Null, 1.5f64.to_string().as_str().into()].into()),
        ]);
        let s = j.to_string();
        assert_eq!(Json::parse(&s).unwrap(), j);
    }

    #[test]
    fn errors() {
        assert!(Json::parse("{").is_err());
        assert!(Json::parse("[1,]").is_err());
        assert!(Json::parse("1 2").is_err());
    }
}
