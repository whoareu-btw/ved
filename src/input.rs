//! Turns raw terminal bytes into key events. Raw bytes are kept alongside each
//! event so the embedded terminal can forward exactly what the host terminal sent.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    /// Ctrl + letter (lowercase) or one of `\\ ] ^ _` and space.
    Ctrl(char),
    Alt(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Insert,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
    Paste(String),
    Mouse(Mouse),
    Unknown,
}

/// A mouse event in the SGR encoding (`ESC [ < b ; x ; y M` for a press, `m` for a release).
/// Positions are 0-based screen cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mouse {
    /// the button number as the terminal reports it: 0 left, 1 middle, 2 right; +4 shift, +8 meta,
    /// +16 ctrl, +32 motion, +64 wheel (64 up, 65 down)
    pub button: u16,
    pub row: u16,
    pub col: u16,
    pub release: bool,
}

impl Mouse {
    pub fn is_wheel(&self) -> bool {
        self.button & 64 != 0
    }
    pub fn wheel_up(&self) -> bool {
        self.button & 67 == 64
    }
    pub fn wheel_down(&self) -> bool {
        self.button & 67 == 65
    }
    pub fn motion(&self) -> bool {
        self.button & 32 != 0
    }
    /// A press of the middle button.
    pub fn middle_press(&self) -> bool {
        !self.release && !self.is_wheel() && !self.motion() && self.button & 3 == 1
    }
    /// The mouse moving with the main button held down (a drag).
    pub fn left_drag(&self) -> bool {
        !self.release && !self.is_wheel() && self.motion() && self.button & 3 == 0
    }
    /// A press of the main button (modifiers are ignored).
    pub fn left_press(&self) -> bool {
        !self.release && !self.is_wheel() && !self.motion() && self.button & 3 == 0
    }
}

/// `params` is what follows `ESC [ <`: `b;x;y`.
fn sgr_mouse(params: &str, release: bool) -> Key {
    let parts: Vec<Option<u32>> = params.split(';').map(|s| s.parse().ok()).collect();
    match parts[..] {
        [Some(b), Some(x), Some(y)] if b < 256 && x >= 1 && y >= 1 => Key::Mouse(Mouse { button: b as u16, col: (x - 1).min(65535) as u16, row: (y - 1).min(65535) as u16, release }),
        _ => Key::Unknown,
    }
}

#[derive(Clone, Debug)]
pub struct Event {
    pub key: Key,
    pub raw: Vec<u8>,
}

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Default)]
pub struct InputParser {
    buf: Vec<u8>,
    in_paste: bool,
}

enum Parsed {
    Need,
    Got(Key, usize),
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

fn decode_char(buf: &[u8]) -> Option<(char, usize)> {
    let n = utf8_len(buf[0]);
    if buf.len() < n {
        return None;
    }
    match std::str::from_utf8(&buf[..n]) {
        Ok(s) => Some((s.chars().next().unwrap(), n)),
        Err(_) => Some(('\u{FFFD}', 1)),
    }
}

fn csi_key(params: &str, fin: u8) -> Key {
    let first: u32 = params.split(';').next().and_then(|p| p.parse().ok()).unwrap_or(0);
    match fin {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'C' => Key::Right,
        b'D' => Key::Left,
        b'H' => Key::Home,
        b'F' => Key::End,
        b'Z' => Key::BackTab,
        b'~' => match first {
            1 | 7 => Key::Home,
            2 => Key::Insert,
            3 => Key::Delete,
            4 | 8 => Key::End,
            5 => Key::PageUp,
            6 => Key::PageDown,
            11..=15 => Key::F((first - 10) as u8),
            17..=21 => Key::F((first - 11) as u8),
            23 | 24 => Key::F((first - 12) as u8),
            _ => Key::Unknown,
        },
        _ => Key::Unknown,
    }
}

impl InputParser {
    pub fn new() -> InputParser {
        InputParser::default()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// True when buffered bytes are waiting for more input (e.g. a lone ESC).
    pub fn has_pending(&self) -> bool {
        !self.buf.is_empty()
    }

    fn parse_one(&self, flush: bool) -> Parsed {
        let b = &self.buf;
        match b[0] {
            0x1b => {
                if b.len() == 1 {
                    return if flush { Parsed::Got(Key::Esc, 1) } else { Parsed::Need };
                }
                match b[1] {
                    b'[' => {
                        let mut i = 2;
                        while i < b.len() && !(0x40..=0x7E).contains(&b[i]) {
                            i += 1;
                        }
                        if i >= b.len() {
                            return if flush { Parsed::Got(Key::Unknown, b.len()) } else { Parsed::Need };
                        }
                        let params = String::from_utf8_lossy(&b[2..i]).to_string();
                        if let (Some(rest), b'M' | b'm') = (params.strip_prefix('<'), b[i]) {
                            return Parsed::Got(sgr_mouse(rest, b[i] == b'm'), i + 1);
                        }
                        Parsed::Got(csi_key(&params, b[i]), i + 1)
                    }
                    b'O' => {
                        if b.len() < 3 {
                            return if flush { Parsed::Got(Key::Unknown, 2) } else { Parsed::Need };
                        }
                        let k = match b[2] {
                            b'A' => Key::Up,
                            b'B' => Key::Down,
                            b'C' => Key::Right,
                            b'D' => Key::Left,
                            b'H' => Key::Home,
                            b'F' => Key::End,
                            b'P' => Key::F(1),
                            b'Q' => Key::F(2),
                            b'R' => Key::F(3),
                            b'S' => Key::F(4),
                            _ => Key::Unknown,
                        };
                        Parsed::Got(k, 3)
                    }
                    0x1b => Parsed::Got(Key::Esc, 1),
                    _ => match decode_char(&b[1..]) {
                        Some((c, n)) => Parsed::Got(Key::Alt(c), 1 + n),
                        None => {
                            if flush {
                                Parsed::Got(Key::Unknown, b.len())
                            } else {
                                Parsed::Need
                            }
                        }
                    },
                }
            }
            0x0d => Parsed::Got(Key::Enter, 1),
            0x09 => Parsed::Got(Key::Tab, 1),
            0x7f | 0x08 => Parsed::Got(Key::Backspace, 1),
            0x00 => Parsed::Got(Key::Ctrl(' '), 1),
            c @ 0x01..=0x1a => Parsed::Got(Key::Ctrl((b'a' + c - 1) as char), 1),
            0x1c => Parsed::Got(Key::Ctrl('\\'), 1),
            0x1d => Parsed::Got(Key::Ctrl(']'), 1),
            0x1e => Parsed::Got(Key::Ctrl('^'), 1),
            0x1f => Parsed::Got(Key::Ctrl('_'), 1),
            _ => match decode_char(b) {
                Some((c, n)) => Parsed::Got(Key::Char(c), n),
                None => {
                    if flush {
                        Parsed::Got(Key::Unknown, b.len())
                    } else {
                        Parsed::Need
                    }
                }
            },
        }
    }

    /// Next complete event. With `flush`, incomplete sequences are resolved
    /// (a lone ESC becomes `Key::Esc`).
    pub fn next_event(&mut self, flush: bool) -> Option<Event> {
        if self.buf.is_empty() {
            return None;
        }
        if self.in_paste {
            let pos = self.buf.windows(PASTE_END.len()).position(|w| w == PASTE_END)?;
            let text = String::from_utf8_lossy(&self.buf[..pos]).to_string();
            let mut raw = PASTE_START.to_vec();
            raw.extend_from_slice(&self.buf[..pos]);
            raw.extend_from_slice(PASTE_END);
            self.buf.drain(..pos + PASTE_END.len());
            self.in_paste = false;
            return Some(Event { key: Key::Paste(text), raw });
        }
        if self.buf.starts_with(PASTE_START) {
            self.buf.drain(..PASTE_START.len());
            self.in_paste = true;
            return self.next_event(flush);
        }
        if PASTE_START.starts_with(&self.buf) && !flush {
            return None; // could still become a paste start
        }
        match self.parse_one(flush) {
            Parsed::Need => None,
            Parsed::Got(key, n) => {
                let raw: Vec<u8> = self.buf.drain(..n).collect();
                Some(Event { key, raw })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(bytes: &[u8]) -> Vec<Key> {
        let mut p = InputParser::new();
        p.push(bytes);
        let mut v = vec![];
        while let Some(e) = p.next_event(true) {
            v.push(e.key);
        }
        v
    }

    #[test]
    fn basic_keys() {
        assert_eq!(keys(b"a\r\x1b"), vec![Key::Char('a'), Key::Enter, Key::Esc]);
        assert_eq!(keys(b"\x03"), vec![Key::Ctrl('c')]);
        assert_eq!(keys(b"\x1b[A\x1b[3~"), vec![Key::Up, Key::Delete]);
        assert_eq!(keys("é".as_bytes()), vec![Key::Char('é')]);
        assert_eq!(keys(b"\x1bx"), vec![Key::Alt('x')]);
        assert_eq!(keys(b"\x1b[1;5C"), vec![Key::Right]);
    }

    #[test]
    fn lone_esc_waits_until_flush() {
        let mut p = InputParser::new();
        p.push(b"\x1b");
        assert!(p.next_event(false).is_none());
        assert_eq!(p.next_event(true).unwrap().key, Key::Esc);
    }

    #[test]
    fn split_sequence() {
        let mut p = InputParser::new();
        p.push(b"\x1b[");
        assert!(p.next_event(false).is_none());
        p.push(b"B");
        assert_eq!(p.next_event(false).unwrap().key, Key::Down);
    }

    #[test]
    fn bracketed_paste() {
        let k = keys(b"\x1b[200~hello\nworld\x1b[201~x");
        assert_eq!(k, vec![Key::Paste("hello\nworld".into()), Key::Char('x')]);
    }

    #[test]
    fn sgr_mouse_events() {
        let m = |b: u16, col: u16, row: u16, release: bool| Key::Mouse(Mouse { button: b, col, row, release });
        assert_eq!(keys(b"\x1b[<0;1;1M"), vec![m(0, 0, 0, false)]);
        assert_eq!(keys(b"\x1b[<0;12;7m"), vec![m(0, 11, 6, true)]);
        assert_eq!(keys(b"\x1b[<64;40;3M"), vec![m(64, 39, 2, false)]);
        assert_eq!(keys(b"\x1b[<65;1;1M\x1b[<0;2;2Mx"), vec![m(65, 0, 0, false), m(0, 1, 1, false), Key::Char('x')]);
        // nonsense is swallowed whole, never turned into typed text
        for bad in [&b"\x1b[<M"[..], b"\x1b[<0;0;0M", b"\x1b[<0;1M", b"\x1b[<999;1;1M", b"\x1b[<0;1;1;1M", b"\x1b[<-1;1;1M"] {
            assert_eq!(keys(bad), vec![Key::Unknown], "{:?}", String::from_utf8_lossy(bad));
        }
        // a letter ends an escape sequence (here it is not a mouse report): nothing is lost or invented
        assert_eq!(keys(b"\x1b[<a")[0], Key::Unknown);
        // huge positions are clamped
        assert_eq!(keys(b"\x1b[<0;99999999999;4294967296M"), vec![Key::Unknown]);
        assert_eq!(keys(b"\x1b[<0;70000;70000M"), vec![m(0, 65535, 65535, false)]);
        let wheel = Mouse { button: 64, row: 0, col: 0, release: false };
        assert!(wheel.wheel_up() && wheel.is_wheel() && !wheel.left_press());
        assert!(Mouse { button: 0, ..wheel }.left_press());
        assert!(Mouse { button: 4, ..wheel }.left_press(), "a modifier does not stop a click");
        assert!(!Mouse { button: 32, ..wheel }.left_press(), "dragging is not a click");
        assert!(!Mouse { button: 2, ..wheel }.left_press());
    }
}
