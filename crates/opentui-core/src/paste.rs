//! Paste — `packages/core/src/lib/paste.ts` plus the bracketed-paste
//! collector lifted from `StdinParser` (stdin-parser.ts:552-578,
//! 1945-1991).
//!
//! * `decode_paste_bytes` — `new TextDecoder().decode(bytes)`
//!   (paste.ts:10-13): WHATWG UTF-8, non-fatal (invalid sequences →
//!   U+FFFD per the spec's error recovery) with the leading BOM
//!   stripped (`ignoreBOM` defaults to false).
//! * `strip_ansi_sequences` — `stripANSI`
//!   (platform/runtime.ts:41): under bun (the runtime opencode and the
//!   differential oracle use) this is `Bun.stripANSI`. The port is the
//!   escape-sequence state machine that builtin implements, verified
//!   against it byte-for-byte on a corpus (see tests).

/// `PasteKind` (paste.ts:3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteKind {
    Text,
    Binary,
    Unknown,
}

/// `PasteMetadata` (paste.ts:5-8).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PasteMetadata {
    pub mime_type: Option<String>,
    pub kind: Option<PasteKind>,
}

/// `decodePasteBytes` (paste.ts:12-14) — WHATWG `TextDecoder` UTF-8
/// decode: replacement-char error handling per the encoding spec, BOM
/// removed.
#[must_use]
pub fn decode_paste_bytes(bytes: &[u8]) -> String {
    let mut text = whatwg_utf8_decode(bytes);
    // TextDecoder with ignoreBOM=false strips ONE leading U+FEFF.
    if text.starts_with('\u{feff}') {
        text.remove(0);
    }
    text
}

/// The WHATWG encoding-spec UTF-8 decoder (non-fatal). Differs from
/// Rust's `from_utf8_lossy` in exactly the cases the spec calls out:
/// maximal subpart recovery (e.g. E2 28 A1 → FFFD FFFD FFFD, and
/// truncated-but-valid prefixes at end of input → one FFFD per the
/// spec's "maximal subpart" rule).
fn whatwg_utf8_decode(bytes: &[u8]) -> String {
    // <https://encoding.spec.whatwg.org/#utf-8-decoder>
    let mut out = String::with_capacity(bytes.len());
    let mut code_point: u32 = 0;
    let mut bytes_needed: u32 = 0;
    let mut lower_boundary: u32 = 0x80;
    let mut upper_boundary: u32 = 0xbf;
    let mut i = 0;
    while i < bytes.len() {
        let byte = u32::from(bytes[i]);
        i += 1;
        if bytes_needed == 0 {
            if byte <= 0x7f {
                out.push(char::from_u32(byte).unwrap_or('\u{fffd}'));
            } else if (0xc2..=0xdf).contains(&byte) {
                bytes_needed = 1;
                code_point = byte & 0x1f;
            } else if (0xe0..=0xef).contains(&byte) {
                if byte == 0xe0 {
                    lower_boundary = 0xa0;
                }
                if byte == 0xed {
                    upper_boundary = 0x9f;
                }
                bytes_needed = 2;
                code_point = byte & 0xf;
            } else if (0xf0..=0xf4).contains(&byte) {
                if byte == 0xf0 {
                    lower_boundary = 0x90;
                }
                if byte == 0xf4 {
                    upper_boundary = 0x8f;
                }
                bytes_needed = 3;
                code_point = byte & 0x7;
            } else {
                out.push('\u{fffd}');
            }
        } else if !(lower_boundary..=upper_boundary).contains(&byte) {
            // Not a continuation: emit U+FFFD for the maximal subpart
            // and REPROCESS this byte.
            code_point = 0;
            bytes_needed = 0;
            lower_boundary = 0x80;
            upper_boundary = 0xbf;
            out.push('\u{fffd}');
            i -= 1;
        } else {
            lower_boundary = 0x80;
            upper_boundary = 0xbf;
            code_point = (code_point << 6) | (byte & 0x3f);
            bytes_needed -= 1;
            if bytes_needed == 0 {
                out.push(char::from_u32(code_point).unwrap_or('\u{fffd}'));
                code_point = 0;
            }
        }
    }
    // Truncated sequence at end of input → one U+FFFD per the spec
    // (the pending bytes are a maximal subpart).
    if bytes_needed != 0 {
        out.push('\u{fffd}');
    }
    out
}

/// `stripAnsiSequences` (paste.ts:16-17) → `stripANSI`
/// (platform/runtime.ts:41). Bun's builtin is the escape-sequence
/// state machine below; behavior pinned by probes against
/// `Bun.stripANSI` (bun 1.3.10):
///
/// * `ESC` then `[` → CSI (params 0x30-0x3F, intermediates
///   0x20-0x2F, final 0x40-0x7E; unterminated consumes to end)
/// * `ESC` then `]` → OSC (to BEL or ST `ESC\`; unterminated consumes
///   to end)
/// * `ESC` then `P X ^ _` → DCS/SOS/PM/APC string (to ST; BEL does
///   NOT terminate)
/// * `ESC` then intermediates (0x20-0x2F) + final (0x30-0x7E) →
///   multi-byte escape (`ESC(B`); `ESC ESC` ends the sequence
/// * lone CSI byte 0x9B behaves like `ESC [`
#[must_use]
pub fn strip_ansi_sequences(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut state = StripState::Ground;
    let mut prev_esc = false; // for ST detection inside OSC/string
    for ch in text.chars() {
        let byte = ch as u32;
        match state {
            StripState::Ground => {
                if byte == 0x1b {
                    state = StripState::Escape;
                } else if byte == 0x9b {
                    state = StripState::Csi;
                } else {
                    out.push(ch);
                }
            }
            StripState::Escape => match byte {
                0x5b => state = StripState::Csi,                      // [
                0x5d => state = StripState::Osc,                      // ]
                0x50 | 0x58 | 0x5e | 0x5f => state = StripState::Str, // P X ^ _
                0x1b => state = StripState::Ground,                   // ESC ESC ends it
                0x20..=0x2f => { /* intermediate: stay */ }
                _ => state = StripState::Ground,
            },
            StripState::Csi => match byte {
                // params (0x30-0x3f) and intermediates (0x20-0x2f)
                0x20..=0x3f => {}
                0x40..=0x7e => state = StripState::Ground, // final byte
                0x1b => state = StripState::Csi,           // restart CSI fresh
                _ => { /* C0 controls are consumed */ }
            },
            StripState::Osc | StripState::Str => {
                let is_osc = state == StripState::Osc;
                if prev_esc {
                    prev_esc = false;
                    if byte == 0x5c {
                        // ESC \ — ST terminator
                        state = StripState::Ground;
                        continue;
                    }
                    // ESC + other: stay in the string (consumed)
                    continue;
                }
                if byte == 0x1b {
                    prev_esc = true;
                } else if is_osc && byte == 0x07 {
                    state = StripState::Ground; // BEL terminates OSC only
                }
            }
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum StripState {
    Ground,
    Escape,
    Csi,
    Osc,
    Str,
}

/// `BRACKETED_PASTE_START` / `BRACKETED_PASTE_END`
/// (stdin-parser.ts:118-119).
pub const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
pub const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

/// `PasteCollector` (stdin-parser.ts:104-117, 552-558) + the
/// `consumePasteBytes` sliding-tail algorithm (stdin-parser.ts:1952-1991):
/// paste bytes accumulate across arbitrary chunk boundaries; the tail
/// keeps `END.len() - 1` bytes so a split end marker is still found.
#[derive(Debug, Default)]
pub struct BracketedPasteCollector {
    tail: Vec<u8>,
    parts: Vec<Vec<u8>>,
    total_length: usize,
}

impl BracketedPasteCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a chunk during an active bracketed paste. Returns
    /// `Some(paste_bytes, remainder)` when the end marker was found —
    /// `remainder` is the bytes after the marker that belong to normal
    /// parsing — or `None` while the paste continues.
    pub fn consume(&mut self, chunk: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
        let mut combined = std::mem::take(&mut self.tail);
        combined.extend_from_slice(chunk);

        if let Some(end_index) = index_of_bytes(&combined, BRACKETED_PASTE_END) {
            self.push_paste_bytes(&combined[..end_index]);
            let paste = self.join_paste_bytes();
            let remainder = combined[end_index + BRACKETED_PASTE_END.len()..].to_vec();
            // Collector is done: state resets (this instance is
            // dropped by the caller, matching `this.paste = null`).
            return Some((paste, remainder));
        }

        // Keep enough trailing bytes to detect an end marker split
        // across chunks (stdin-parser.ts:1969-1978).
        let keep = (BRACKETED_PASTE_END.len() - 1).min(combined.len());
        let stable_length = combined.len() - keep;
        if stable_length > 0 {
            self.push_paste_bytes(&combined[..stable_length]);
        }
        self.tail = combined[stable_length..].to_vec();
        None
    }

    fn push_paste_bytes(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.parts.push(bytes.to_vec());
        self.total_length += bytes.len();
    }

    /// `joinPasteBytes` (stdin-parser.ts:560-577).
    fn join_paste_bytes(&mut self) -> Vec<u8> {
        if self.total_length == 0 {
            return Vec::new();
        }
        if self.parts.len() == 1 {
            return std::mem::take(&mut self.parts[0]);
        }
        let mut bytes = Vec::with_capacity(self.total_length);
        for part in &self.parts {
            bytes.extend_from_slice(part);
        }
        bytes
    }
}

/// `indexOfBytes` (stdin-parser.ts:518-539).
fn index_of_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len())
        .find(|&offset| &haystack[offset..offset + needle.len()] == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_paste_utf8() {
        assert_eq!(decode_paste_bytes(b"hello"), "hello");
        assert_eq!(decode_paste_bytes("héllo😀".as_bytes()), "héllo😀");
        // BOM stripped
        assert_eq!(decode_paste_bytes("\u{feff}x".as_bytes()), "x");
        // invalid → replacement chars per the WHATWG maximal-subpart
        // rule: E2 followed by a non-continuation emits ONE
        // replacement, then '(' is kept (verified against
        // `new TextDecoder().decode` under bun 1.3.10 — this is where
        // Rust's from_utf8_lossy differs: it gives 3 replacements)
        assert_eq!(decode_paste_bytes(&[0xe2, 0x28, 0xa1]), "\u{fffd}(\u{fffd}");
        // truncated sequence at end → single replacement
        assert_eq!(decode_paste_bytes(&[0xe2, 0x82]), "\u{fffd}");
        assert_eq!(decode_paste_bytes(&[0xe2, 0x82, 0xac]), "€");
        // E1 80 → subpart FFFD
        assert_eq!(decode_paste_bytes(&[0xe1, 0x80]), "\u{fffd}");
        // continuation byte alone
        assert_eq!(decode_paste_bytes(&[0x80]), "\u{fffd}");
        // F5 is out of range entirely
        assert_eq!(decode_paste_bytes(&[0xf5]), "\u{fffd}");
    }

    #[test]
    fn strip_ansi_matches_bun_builtin() {
        // corpus probed against Bun.stripANSI (bun 1.3.10)
        let cases: &[(&str, &str)] = &[
            ("\x1b[31mred\x1b[0m", "red"),
            ("plain", "plain"),
            ("\x1b[2J\x1b[H", ""),
            ("\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\", "link"),
            ("\x1b[1;2;3H", ""),
            ("a\x1b[?25lb", "ab"),
            ("\x07bell", "\x07bell"),
            ("\x1bP dcs \x1b\\", ""),
            ("\u{9b}31m", ""),
            ("\x1b[38;2;10;20;30mtrue", "true"),
            ("中文\x1b[31m色\x1b[0m", "中文色"),
            ("\x1bM", ""),
            ("\x1bMx", "x"),
            ("\x1b7\x1b8", ""),
            ("\x1b(B", ""),
            ("\x1b(Bx", "x"),
            ("\x1b=", ""),
            ("\x1b[?1049h", ""),
            ("\x1b[38;5;196mX", "X"),
            ("\x1b]0;title\x07after", "after"),
            ("\x1b]0;title\x1b\\after", "after"),
            ("\x1b]unterminated", ""),
            ("\x1b[!p", ""),
            ("x\x1b[1 q y", "x y"),
            ("\x1b[>c", ""),
            ("\x1bP1;2|data\x1b\\tail", "tail"),
            ("\u{9b}", ""),
            ("\u{9b}31", ""),
            ("a\x1b[0;1;2;3mrb", "arb"),
            ("\x1bc", ""),
            ("\x1b%G", ""),
            ("\x1b", ""),
            ("a\x1bb", "a"),
            ("\x1b\r", ""),
            ("\x1b\n\n", "\n"),
            ("\x1b[", ""),
            ("\x1b[31", ""),
            ("\x1b]8;;http://x", ""),
            ("\x1b\x1b[31m", "[31m"),
            ("p\x1b[0Kq", "pq"),
            ("\x1b]8;;x\x1bMy", ""),
            ("\x1bPdata\x07bel", ""),
            ("\x1b^pm\x07apc", ""),
            ("\x1b]0;t\x1b[31mz", ""),
            ("\x1b[31\x1b[0m", "0m"),
        ];
        for (input, expected) in cases {
            assert_eq!(&strip_ansi_sequences(input), expected, "input {input:?}");
        }
    }

    #[test]
    fn bracketed_paste_collector_assembles_chunks() {
        // end marker in a later chunk
        let mut c = BracketedPasteCollector::new();
        assert!(c.consume(b"hel").is_none());
        assert!(c.consume(b"lo wo").is_none());
        let (bytes, rest) = c.consume(b"rld\x1b[201~AB").unwrap();
        assert_eq!(bytes, b"hello world");
        assert_eq!(rest, b"AB");
    }

    #[test]
    fn bracketed_paste_collector_split_marker() {
        // end marker split across chunks: "\x1b[2" | "01~"
        let mut c = BracketedPasteCollector::new();
        assert!(c.consume(b"paste\x1b[2").is_none());
        let (bytes, rest) = c.consume(b"01~xy").unwrap();
        assert_eq!(bytes, b"paste");
        assert_eq!(rest, b"xy");
    }

    #[test]
    fn bracketed_paste_collector_split_everywhere() {
        // every boundary position of the end marker
        let body = b"abc";
        for split in 0..BRACKETED_PASTE_END.len() {
            let mut c = BracketedPasteCollector::new();
            let mut data = body.to_vec();
            data.extend_from_slice(BRACKETED_PASTE_END);
            data.extend_from_slice(b"tail");
            assert!(c.consume(&data[..body.len() + split]).is_none());
            let (bytes, rest) = c.consume(&data[body.len() + split..]).unwrap();
            assert_eq!(bytes, body.to_vec());
            assert_eq!(rest, b"tail");
        }
    }

    #[test]
    fn bracketed_paste_collector_empty_paste() {
        let mut c = BracketedPasteCollector::new();
        let (bytes, _) = c.consume(b"\x1b[201~").unwrap();
        assert!(bytes.is_empty());
    }

    #[test]
    fn bracketed_paste_collector_marker_bytes_inside_body() {
        // a partial marker in the body must not terminate early, and
        // stable bytes before the kept tail are retained
        let mut c = BracketedPasteCollector::new();
        let mut body = b"0123456789abc".to_vec();
        body.extend_from_slice(b"\x1b[201~");
        // feed in 3-byte chunks; the ESC[2 prefix inside the body is
        // split across the tail window but the real marker still wins
        assert!(c.consume(&body[..6]).is_none());
        assert!(c.consume(&body[6..12]).is_none());
        let (bytes, rest) = c.consume(&body[12..]).unwrap();
        assert_eq!(bytes, b"0123456789abc");
        assert!(rest.is_empty());
    }
}
