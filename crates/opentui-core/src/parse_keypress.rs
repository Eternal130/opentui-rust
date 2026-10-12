//! `parseKeypress` — the legacy escape-sequence keypress parser
//! (`packages/core/src/lib/parse.keypress.ts`, itself adapted from
//! enquirer's keypress.js; header comment parse.keypress.ts:1).
//!
//! Ported by source translation: the parser tables ([`KEY_NAME`],
//! [`SS3_NUMPAD_PRINTABLE`]), the branch order of the main chain, and
//! the meta/ctrl key-name mapping ([`get_ctrl_key_name`]) are exact.
//! The JS regexes are hand-rolled matchers with identical semantics
//! (documented per function).

/// `KeyEventType` (parse.keypress.ts:159).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEventType {
    Press,
    Repeat,
    Release,
}

impl KeyEventType {
    /// The wire name (`"press" | "repeat" | "release"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            KeyEventType::Press => "press",
            KeyEventType::Repeat => "repeat",
            KeyEventType::Release => "release",
        }
    }
}

/// Where a key event was parsed from — `ParsedKey.source`
/// (parse.keypress.ts:171).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Raw,
    Kitty,
}

impl KeySource {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            KeySource::Raw => "raw",
            KeySource::Kitty => "kitty",
        }
    }
}

/// `ParsedKey` (parse.keypress.ts:161-182). The always-present fields
/// are plain; the optional TS fields (`code`, `super`, `hyper`,
/// `capsLock`, `numLock`, `baseCode`, `repeated`) are `Option` so a
/// characterization diff can tell "unset" from `false`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedKey {
    pub name: String,
    pub ctrl: bool,
    pub meta: bool,
    pub shift: bool,
    pub option: bool,
    pub sequence: String,
    pub number: bool,
    pub raw: String,
    pub event_type: KeyEventType,
    pub source: KeySource,
    pub code: Option<String>,
    pub super_key: Option<bool>,
    pub hyper: Option<bool>,
    pub caps_lock: Option<bool>,
    pub num_lock: Option<bool>,
    /// Kitty's base-layout codepoint (`baseCode`, parse.keypress.ts:180).
    pub base_code: Option<u32>,
    pub repeated: Option<bool>,
}

impl ParsedKey {
    /// The blank key every parse starts from (parse.keypress.ts:293-304).
    fn blank(sequence: &str, source: KeySource) -> Self {
        ParsedKey {
            name: String::new(),
            ctrl: false,
            meta: false,
            shift: false,
            option: false,
            number: false,
            sequence: sequence.to_owned(),
            raw: sequence.to_owned(),
            event_type: KeyEventType::Press,
            source,
            code: None,
            super_key: None,
            hyper: None,
            caps_lock: None,
            num_lock: None,
            base_code: None,
            repeated: None,
        }
    }
}

/// `ParseKeypressOptions` (parse.keypress.ts:184-186).
#[derive(Debug, Clone, Copy, Default)]
pub struct ParseKeypressOptions {
    pub use_kitty_keyboard: bool,
}

/// `keyName` (parse.keypress.ts:9-123) — exact, insertion order
/// preserved because [`NON_ALPHANUMERIC_KEYS`] exports
/// `Object.values(keyName)` order.
const KEY_NAME: &[(&str, &str)] = &[
    // xterm/gnome ESC O letter
    ("OP", "f1"),
    ("OQ", "f2"),
    ("OR", "f3"),
    ("OS", "f4"),
    // xterm/rxvt ESC [ number ~
    ("[11~", "f1"),
    ("[12~", "f2"),
    ("[13~", "f3"),
    ("[14~", "f4"),
    // from Cygwin and used in libuv
    ("[[A", "f1"),
    ("[[B", "f2"),
    ("[[C", "f3"),
    ("[[D", "f4"),
    ("[[E", "f5"),
    // common
    ("[15~", "f5"),
    ("[17~", "f6"),
    ("[18~", "f7"),
    ("[19~", "f8"),
    ("[20~", "f9"),
    ("[21~", "f10"),
    ("[23~", "f11"),
    ("[24~", "f12"),
    ("[29~", "menu"),
    ("[57427~", "clear"),
    // xterm ESC [ letter
    ("[A", "up"),
    ("[B", "down"),
    ("[C", "right"),
    ("[D", "left"),
    ("[E", "clear"),
    ("[F", "end"),
    ("[H", "home"),
    ("[P", "f1"),
    ("[Q", "f2"),
    ("[S", "f4"),
    // xterm/gnome ESC O letter
    ("OA", "up"),
    ("OB", "down"),
    ("OC", "right"),
    ("OD", "left"),
    ("OE", "clear"),
    ("OF", "end"),
    ("OH", "home"),
    // VT100 application keypad (SS3) — DECKPAM (parse.keypress.ts:56-59)
    ("OM", "return"),
    ("Oj", "*"),
    ("Ok", "+"),
    ("Ol", ","),
    ("Om", "-"),
    ("On", "."),
    ("Oo", "/"),
    ("Op", "0"),
    ("Oq", "1"),
    ("Or", "2"),
    ("Os", "3"),
    ("Ot", "4"),
    ("Ou", "5"),
    ("Ov", "6"),
    ("Ow", "7"),
    ("Ox", "8"),
    ("Oy", "9"),
    ("OX", "="),
    // xterm/rxvt ESC [ number ~
    ("[1~", "home"),
    ("[2~", "insert"),
    ("[3~", "delete"),
    ("[4~", "end"),
    ("[5~", "pageup"),
    ("[6~", "pagedown"),
    // putty
    ("[[5~", "pageup"),
    ("[[6~", "pagedown"),
    // rxvt
    ("[7~", "home"),
    ("[8~", "end"),
    // rxvt keys with modifiers
    ("[a", "up"),
    ("[b", "down"),
    ("[c", "right"),
    ("[d", "left"),
    ("[e", "clear"),
    // option + arrow keys (old style)
    ("f", "right"),
    ("b", "left"),
    ("p", "up"),
    ("n", "down"),
    ("[2$", "insert"),
    ("[3$", "delete"),
    ("[5$", "pageup"),
    ("[6$", "pagedown"),
    ("[7$", "home"),
    ("[8$", "end"),
    ("Oa", "up"),
    ("Ob", "down"),
    ("Oc", "right"),
    ("Od", "left"),
    ("Oe", "clear"),
    ("[2^", "insert"),
    ("[3^", "delete"),
    ("[5^", "pageup"),
    ("[6^", "pagedown"),
    ("[7^", "home"),
    ("[8^", "end"),
    // misc.
    ("[Z", "tab"),
];

fn key_name_lookup(code: &str) -> Option<&'static str> {
    KEY_NAME.iter().find(|(k, _)| *k == code).map(|(_, v)| *v)
}

/// `nonAlphanumericKeys` (parse.keypress.ts:125) —
/// `[...Object.values(keyName), "backspace"]`, duplicates included.
pub fn non_alphanumeric_keys() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = KEY_NAME.iter().map(|(_, name)| *name).collect();
    v.push("backspace");
    v
}

/// `terminalNamedSingleStrokeKeys` (parse.keypress.ts:127-129) — the
/// `Set` dedup of the fixed head + both tables, first-occurrence order.
pub fn terminal_named_single_stroke_keys() -> Vec<&'static str> {
    let mut seen: Vec<&'static str> = Vec::new();
    let mut push = |k: &'static str| {
        if !seen.contains(&k) {
            seen.push(k);
        }
    };
    for k in ["return", "linefeed", "tab", "escape", "space"] {
        push(k);
    }
    for k in non_alphanumeric_keys() {
        push(k);
    }
    for k in crate::parse_keypress_kitty::kitty_named_single_stroke_keys() {
        push(k);
    }
    seen
}

/// `isShiftKey` (parse.keypress.ts:131-133).
fn is_shift_key(code: &str) -> bool {
    matches!(
        code,
        "[a" | "[b" | "[c" | "[d" | "[e" | "[2$" | "[3$" | "[5$" | "[6$" | "[7$" | "[8$" | "[Z"
    )
}

/// `isCtrlKey` (parse.keypress.ts:135-137).
fn is_ctrl_key(code: &str) -> bool {
    matches!(
        code,
        "Oa" | "Ob" | "Oc" | "Od" | "Oe" | "[2^" | "[3^" | "[5^" | "[6^" | "[7^" | "[8^"
    )
}

/// `getCtrlKeyName` (parse.keypress.ts:143-157): Ctrl+A..Z as 1..26,
/// Ctrl+space as 0, and the 0x1c..0x1f block mapping to `\ ] ^ _`.
fn get_ctrl_key_name(char_code: u32) -> Option<char> {
    if char_code == 0 {
        // mapped to the name "space" by the caller
        return Some(' ');
    }
    if (1..=26).contains(&char_code) {
        return char::from_u32(char_code + u32::from('a') - 1);
    }
    if (28..=31).contains(&char_code) {
        return char::from_u32(char_code + 64);
    }
    None
}

/// `ss3NumpadPrintable` (parse.keypress.ts:192-210).
const SS3_NUMPAD_PRINTABLE: &[(&str, &str)] = &[
    ("Op", "0"),
    ("Oq", "1"),
    ("Or", "2"),
    ("Os", "3"),
    ("Ot", "4"),
    ("Ou", "5"),
    ("Ov", "6"),
    ("Ow", "7"),
    ("Ox", "8"),
    ("Oy", "9"),
    ("Oj", "*"),
    ("Ok", "+"),
    ("Ol", ","),
    ("Om", "-"),
    ("On", "."),
    ("Oo", "/"),
    ("OX", "="),
];

fn ss3_numpad_printable(code: &str) -> Option<&'static str> {
    SS3_NUMPAD_PRINTABLE
        .iter()
        .find(|(k, _)| *k == code)
        .map(|(_, v)| *v)
}

/// The captures of `fnKeyRe` (parse.keypress.ts:7):
/// `^(?:\x1b+)(O|N|\[|\[\[)(?:(\d+)(?:;(\d+))?([~^$])|(?:1;)?(\d+)?([a-zA-Z]))`
///
/// JS semantics reproduced:
/// * `\x1b+` — one or more ESC bytes
/// * group1 alternation `O|N|\[|\[\[` with regex backtracking: `\[`
///   is tried first, and only when the rest fails does `\[\[` win
///   (this is what makes `ESC [ [ A` parse as `[[A` → f1).
/// * the first alternative (`digits [;digits] [~^$]`) is tried before
///   the second (`[1;] digits? letter`), with the same backtracking
///   over its internal optional `;(\d+)` part.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FnKeyCaptures {
    /// group 1 — `O | N | [ | [[`
    part1: String,
    /// group 2 — leading digits of the tilde alternative
    part2: Option<String>,
    /// group 3 — modifier digits after `;`
    part3: Option<String>,
    /// group 4 — `~ ^ $` terminator
    part4: Option<char>,
    /// group 5 — digits of the letter alternative (after optional `1;`)
    part5: Option<String>,
    /// group 6 — the final letter
    part6: Option<char>,
}

fn match_fn_key_re(s: &str) -> Option<FnKeyCaptures> {
    let bytes = s.as_bytes();
    // (?:\x1b+) — at least one ESC
    let mut i = 0;
    while i < bytes.len() && bytes[i] == 0x1b {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    let rest = &s[i..];

    // group1 alternatives with backtracking: try in order O, N, `[`, `[[`
    for g1 in ["O", "N", "[", "[["] {
        let Some(after) = rest.strip_prefix(g1) else {
            continue;
        };
        if let Some(caps) = match_fn_key_tail(after, g1) {
            return Some(caps);
        }
    }
    None
}

/// The `(?:(\d+)(?:;(\d+))?([~^$])|(?:1;)?(\d+)?([a-zA-Z]))` part of
/// `fnKeyRe`, with JS-style backtracking (first alternative first, and
/// within it the optional `;(\d+)` tried before being dropped).
fn match_fn_key_tail(rest: &str, part1: &str) -> Option<FnKeyCaptures> {
    let bytes = rest.as_bytes();

    // digits run from the start (JS \d+ is ASCII-only)
    let mut digit_len = 0;
    while digit_len < bytes.len() && bytes[digit_len].is_ascii_digit() {
        digit_len += 1;
    }
    let digits = &rest[..digit_len];

    // first alternative: (\d+) (?:;(\d+))? ([~^$])
    if digit_len > 0 {
        let after_digits = &rest[digit_len..];
        // try with the ;modifier group
        if let Some(semi) = after_digits.strip_prefix(';') {
            let mut mod_len = 0;
            let mod_bytes = semi.as_bytes();
            while mod_len < mod_bytes.len() && mod_bytes[mod_len].is_ascii_digit() {
                mod_len += 1;
            }
            if mod_len > 0 {
                let term = &semi[mod_len..];
                if let Some(term_ch) = term.chars().next() {
                    if matches!(term_ch, '~' | '^' | '$') && term.chars().count() == 1 {
                        return Some(FnKeyCaptures {
                            part1: part1.to_owned(),
                            part2: Some(digits.to_owned()),
                            part3: Some(semi[..mod_len].to_owned()),
                            part4: Some(term_ch),
                            part5: None,
                            part6: None,
                        });
                    }
                }
            }
        }
        // retry without the ;modifier group
        if let Some(term_ch) = after_digits.chars().next() {
            if matches!(term_ch, '~' | '^' | '$') && after_digits.chars().count() == 1 {
                return Some(FnKeyCaptures {
                    part1: part1.to_owned(),
                    part2: Some(digits.to_owned()),
                    part3: None,
                    part4: Some(term_ch),
                    part5: None,
                    part6: None,
                });
            }
        }
    }

    // second alternative: (?:1;)?(\d+)?([a-zA-Z])
    let mut tail = rest;
    let mut part5 = None;
    if let Some(after) = tail.strip_prefix("1;") {
        tail = after;
        let t = tail.as_bytes();
        let mut dlen = 0;
        while dlen < t.len() && t[dlen].is_ascii_digit() {
            dlen += 1;
        }
        if dlen > 0 {
            part5 = Some(tail[..dlen].to_owned());
            tail = &tail[dlen..];
        }
    } else {
        // no "1;" — the digits group still applies
        let t = tail.as_bytes();
        let mut dlen = 0;
        while dlen < t.len() && t[dlen].is_ascii_digit() {
            dlen += 1;
        }
        if dlen > 0 {
            part5 = Some(tail[..dlen].to_owned());
            tail = &tail[dlen..];
        }
    }
    let mut chars = tail.chars();
    if let Some(letter) = chars.next() {
        if letter.is_ascii_alphabetic() && chars.next().is_none() {
            return Some(FnKeyCaptures {
                part1: part1.to_owned(),
                part2: None,
                part3: None,
                part4: None,
                part5,
                part6: Some(letter),
            });
        }
    }
    None
}

/// `metaKeyCodeRe` (parse.keypress.ts:5): `^(?:\x1b)([a-zA-Z0-9])$`.
fn match_meta_key_code_re(s: &str) -> Option<char> {
    let mut chars = s.chars();
    let first = chars.next()?;
    if first != '\x1b' {
        return None;
    }
    let second = chars.next()?;
    if !second.is_ascii_alphanumeric() {
        return None;
    }
    if chars.next().is_some() {
        return None;
    }
    Some(second)
}

/// `modifyOtherKeysRe` (parse.keypress.ts:212):
/// `^\x1b\[27;(\d+);(\d+)~$` — captures (modifier, charCode).
fn match_modify_other_keys_re(s: &str) -> Option<(u32, u32)> {
    let rest = s.strip_prefix("\x1b[27;")?;
    let (modifier, code) = rest.split_once(';')?;
    let code = code.strip_suffix('~')?;
    if modifier.is_empty() || code.is_empty() {
        return None;
    }
    if !modifier.bytes().all(|b| b.is_ascii_digit()) || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((modifier.parse::<u32>().ok()?, code.parse::<u32>().ok()?))
}

/// `/^\x1b\[<\d+;\d+;\d+[Mm]$/` (parse.keypress.ts:232) — complete SGR
/// mouse. The three digit runs are each `\d+` (non-empty).
fn is_complete_sgr_mouse(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("\x1b[<") else {
        return false;
    };
    let (params, term) = if let Some(p) = rest.strip_suffix('M') {
        (p, 'M')
    } else {
        match rest.strip_suffix('m') {
            Some(p) => (p, 'm'),
            None => return false,
        }
    };
    let _ = term;
    let parts: Vec<&str> = params.split(';').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// `/^\[<\d+;\d+;\d+[Mm]$/` (parse.keypress.ts:237) — SGR continuation
/// without the leading ESC.
fn is_complete_sgr_mouse_continuation(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("[<") else {
        return false;
    };
    let params = rest
        .strip_suffix('M')
        .or_else(|| rest.strip_suffix('m'))
        .unwrap_or("");
    if params.len() + 1 != rest.len() {
        return false;
    }
    let parts: Vec<&str> = params.split(';').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// `/^\x1b\[<[\d;]*$/` (parse.keypress.ts:243) — partial SGR mouse
/// (zero or more digits/semicolons, then end).
fn is_partial_sgr_mouse(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("\x1b[<") else {
        return false;
    };
    rest.bytes().all(|b| b.is_ascii_digit() || b == b';')
}

/// `/^\[<[\d;]*$/` (parse.keypress.ts:247) — partial SGR continuation.
fn is_partial_sgr_mouse_continuation(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("[<") else {
        return false;
    };
    rest.bytes().all(|b| b.is_ascii_digit() || b == b';')
}

/// `/^\x1b\[\d+;\d+;\d+t$/` (parse.keypress.ts:258) — window size report.
fn is_window_size_report(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("\x1b[") else {
        return false;
    };
    let Some(params) = rest.strip_suffix('t') else {
        return false;
    };
    let parts: Vec<&str> = params.split(';').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// `/^\x1b\[\d+;\d+R$/` (parse.keypress.ts:263) — DSR cursor report.
fn is_cursor_position_report(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("\x1b[") else {
        return false;
    };
    let Some(params) = rest.strip_suffix('R') else {
        return false;
    };
    let parts: Vec<&str> = params.split(';').collect();
    parts.len() == 2
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// `/^\x1b\[\?[\d;]+c$/` (parse.keypress.ts:268) — DA response.
fn is_device_attributes_response(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("\x1b[?") else {
        return false;
    };
    let Some(params) = rest.strip_suffix('c') else {
        return false;
    };
    !params.is_empty() && params.bytes().all(|b| b.is_ascii_digit() || b == b';')
}

/// `/^\x1b\[\?[\d;]+\$y$/` (parse.keypress.ts:273) — mode report.
fn is_mode_report(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("\x1b[?") else {
        return false;
    };
    let Some(params) = rest.strip_suffix("$y") else {
        return false;
    };
    !params.is_empty() && params.bytes().all(|b| b.is_ascii_digit() || b == b';')
}

/// `/^\x1b\][\d;].*(\x1b\\|\x07)$/` (parse.keypress.ts:284) — OSC
/// response terminated by ST or BEL. `.` does not match JS line
/// terminators, so a body containing one fails the regex.
fn is_osc_response(s: &str) -> bool {
    let mut chars = s.chars();
    if chars.next() != Some('\x1b') || chars.next() != Some(']') {
        return false;
    }
    let second = match chars.next() {
        Some(c) if c.is_ascii_digit() || c == ';' => c,
        _ => return false,
    };
    let _ = second;
    // body = everything after ESC ] <class-char>; must end with ST or BEL
    let body: Vec<char> = s.chars().skip(3).collect();
    let ends_with = |suffix: &[char]| -> bool {
        body.len() >= suffix.len() && body[body.len() - suffix.len()..] == *suffix
    };
    if !ends_with(&['\x1b', '\\']) && !ends_with(&['\u{7}']) {
        return false;
    }
    // `.` excludes line terminators (no /s flag)
    let term_len = if ends_with(&['\x1b', '\\']) { 2 } else { 1 };
    !body[..body.len() - term_len]
        .iter()
        .any(|c| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
}

/// `parseKeypress(s, options)` (parse.keypress.ts:214-480) — the string
/// entry point.
#[must_use]
pub fn parse_keypress(s: &str, options: ParseKeypressOptions) -> Option<ParsedKey> {
    // Buffer-coercion branch is parse_keypress_bytes; here `s` is a str.
    parse_keypress_inner(s, options)
}

/// JS `parseInt(s, 10)` for the modifier fields (leading digits, may
/// exceed u32 in pathological inputs — saturate like a double).
fn js_int(s: &str) -> Option<i64> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<i64>().ok()
}

/// `parseKeypress` with a `Buffer` — the high-byte meta encoding
/// (parse.keypress.ts:217-223): a lone byte > 0x7f becomes
/// `ESC + chr(byte - 128)`; anything else decodes as UTF-8 (Node's
/// `String(buffer)`, which replaces invalid sequences with U+FFFD).
#[must_use]
pub fn parse_keypress_bytes(s: &[u8], options: ParseKeypressOptions) -> Option<ParsedKey> {
    if s.len() == 1 && s[0] > 127 {
        let adjusted = s[0] - 128;
        let seq = format!("\x1b{}", char::from_u32(u32::from(adjusted))?);
        return parse_keypress_inner(&seq, options);
    }
    let decoded = String::from_utf8_lossy(s).into_owned();
    parse_keypress_inner(&decoded, options)
}

fn parse_keypress_inner(s: &str, options: ParseKeypressOptions) -> Option<ParsedKey> {
    // Filter out mouse events (SGR and basic) — parse.keypress.ts:230-252
    if is_complete_sgr_mouse(s) || is_complete_sgr_mouse_continuation(s) {
        return None;
    }
    if is_partial_sgr_mouse(s) || is_partial_sgr_mouse_continuation(s) {
        return None;
    }
    if s.starts_with("\x1b[M") && s.chars().count() >= 6 {
        return None;
    }

    // Terminal response filters — parse.keypress.ts:254-291
    if is_window_size_report(s)
        || is_cursor_position_report(s)
        || is_device_attributes_response(s)
        || is_mode_report(s)
    {
        return None;
    }
    if s == "\x1b[I" || s == "\x1b[O" {
        return None;
    }
    if is_osc_response(s) {
        return None;
    }
    if s == "\x1b[200~" || s == "\x1b[201~" {
        return None;
    }

    let mut key = ParsedKey::blank(s, KeySource::Raw);

    // key.sequence = key.sequence || s || key.name (parse.keypress.ts:306)
    if key.sequence.is_empty() {
        key.sequence = s.to_owned();
    }

    let char_count = s.chars().count();
    let first_char = s.chars().next();
    let ctrl_key_name = if char_count == 1 {
        first_char.and_then(|c| get_ctrl_key_name(c as u32))
    } else {
        None
    };
    let meta_ctrl_key_name = if char_count == 2 && first_char == Some('\x1b') {
        s.chars().nth(1).and_then(|c| get_ctrl_key_name(c as u32))
    } else {
        None
    };

    // Kitty keyboard protocol first — parse.keypress.ts:311-317
    if options.use_kitty_keyboard {
        if let Some(kitty) = crate::parse_keypress_kitty::parse_kitty_keyboard(s) {
            return Some(kitty);
        }
    }

    // modifyOtherKeys — parse.keypress.ts:319-358
    if let Some((modifier, char_code)) = match_modify_other_keys_re(s) {
        let modifier = modifier.saturating_sub(1);
        key.ctrl = modifier & 4 != 0;
        key.meta = modifier & 2 != 0; // Alt/Option sets meta
        key.shift = modifier & 1 != 0;
        key.option = modifier & 2 != 0;
        key.super_key = Some(modifier & 8 != 0);
        key.hyper = Some(modifier & 16 != 0);

        match char_code {
            13 => key.name = "return".into(),
            27 => key.name = "escape".into(),
            9 => key.name = "tab".into(),
            32 => key.name = "space".into(),
            127 | 8 => key.name = "backspace".into(),
            _ => {
                // For other character codes, use the character itself.
                // JS `String.fromCharCode` truncates to 16 bits
                // (parse.keypress.ts:349) — e.g. 128512 becomes U+F600.
                let ch = char::from_u32(char_code & 0xffff)?;
                key.name = ch.to_string();
                key.sequence = ch.to_string();
                if (48..=57).contains(&char_code) {
                    key.number = true;
                }
            }
        }
        return Some(key);
    }

    if s == "\r" || s == "\x1b\r" {
        // carriage return — parse.keypress.ts:360-363
        key.name = "return".into();
        key.meta = char_count == 2;
    } else if s == "\n" || s == "\x1b\n" {
        // linefeed — parse.keypress.ts:364-367
        key.name = "linefeed".into();
        key.meta = char_count == 2;
    } else if s == "\t" {
        key.name = "tab".into();
    } else if s == "\x08" || s == "\x1b\x08" || s == "\x7f" || s == "\x1b\x7f" {
        // backspace or ctrl+h (parse.keypress.ts:371-375)
        key.name = "backspace".into();
        key.meta = first_char == Some('\x1b');
    } else if s == "\x1b" || s == "\x1b\x1b" {
        // escape — parse.keypress.ts:376-379
        key.name = "escape".into();
        key.meta = char_count == 2;
    } else if s == " " || s == "\x1b " {
        key.name = "space".into();
        key.meta = char_count == 2;
    } else if let Some(ctrl_name) = ctrl_key_name {
        // ctrl+space, ctrl+letter, ctrl+\, ctrl+], ctrl+^, ctrl+_
        key.name = if ctrl_name == ' ' {
            "space".to_owned()
        } else {
            ctrl_name.to_string()
        };
        key.ctrl = true;
    } else if char_count == 1 && first_char.is_some_and(|c| c.is_ascii_digit()) {
        // number — keep the actual number character for vim commands
        key.name = s.to_owned();
        key.number = true;
    } else if char_count == 1 && first_char.is_some_and(|c| c.is_ascii_lowercase()) {
        key.name = s.to_owned();
    } else if char_count == 1 && first_char.is_some_and(|c| c.is_ascii_uppercase()) {
        // shift+letter
        key.name = s.to_lowercase();
        key.shift = true;
    } else if char_count == 1 {
        // Single character (including emoji/surrogate pairs above BMP) —
        // parse.keypress.ts:398. A Rust `char` IS one codepoint, which
        // covers both the BMP single-unit and the surrogate-pair cases.
        key.name = s.to_owned();
    } else if let Some(char) = match_meta_key_code_re(s) {
        // meta+character key — parse.keypress.ts:401-417
        key.meta = true;
        let is_upper = char.is_ascii_uppercase();
        if char == 'F' {
            key.name = "right".into();
        } else if char == 'B' {
            key.name = "left".into();
        } else if is_upper {
            key.shift = true;
            key.name = char.to_string();
        } else {
            key.name = char.to_string();
        }
    } else if let Some(meta_ctrl) = meta_ctrl_key_name {
        // ESC + control byte is the raw meta+ctrl form —
        // parse.keypress.ts:418-423
        key.meta = true;
        key.ctrl = true;
        key.name = if meta_ctrl == ' ' {
            "space".to_owned()
        } else {
            meta_ctrl.to_string()
        };
    } else if let Some(parts) = match_fn_key_re(s) {
        // ansi escape sequence — parse.keypress.ts:424-471
        let segs: Vec<char> = s.chars().collect();
        if segs.first() == Some(&'\x1b') && segs.get(1) == Some(&'\x1b') {
            key.option = true;
            key.meta = true;
        }

        // reassemble the key code leaving out leading \x1b's,
        // the modifier key bitflag and any meaningless "1;" sequence
        let mut code = String::new();
        code.push_str(&parts.part1);
        if let Some(p2) = &parts.part2 {
            code.push_str(p2);
        }
        if let Some(p4) = parts.part4 {
            code.push(p4);
        }
        if let Some(p6) = parts.part6 {
            code.push(p6);
        }

        let modifier_str = parts
            .part3
            .clone()
            .or_else(|| parts.part5.clone())
            .unwrap_or_else(|| "1".to_owned());
        // JS `parseInt(...) - 1` can go NEGATIVE (e.g. an explicit "0"
        // modifier → -1); JS bit-ops then use two's complement, where
        // -1 has every modifier bit set. Port with i64 wrapping math.
        let modifier = js_int(&modifier_str).unwrap_or(1).wrapping_sub(1);

        // Terminal modifier bits: 1=Shift, 2=Alt/Option, 4=Ctrl, 8=Super, 16=Hyper
        key.ctrl = key.ctrl || (modifier & 4 != 0);
        key.meta = key.meta || (modifier & 2 != 0);
        key.shift = key.shift || (modifier & 1 != 0);
        key.option = key.option || (modifier & 2 != 0);
        key.super_key = Some(modifier & 8 != 0);
        key.hyper = Some(modifier & 16 != 0);
        key.code = Some(code.clone());

        if let Some(name) = key_name_lookup(&code) {
            key.name = name.to_owned();
            key.shift = is_shift_key(&code) || key.shift;
            key.ctrl = is_ctrl_key(&code) || key.ctrl;

            // SS3 application-keypad override — parse.keypress.ts:456-465
            if let Some(ss3_char) = ss3_numpad_printable(&code) {
                key.sequence = ss3_char.to_owned();
                if key.name.as_str() >= "0" && key.name.as_str() <= "9" {
                    key.number = true;
                }
            }
        } else {
            // Unknown sequence — reset (parse.keypress.ts:466-471)
            key.name = String::new();
            key.code = None;
        }
    } else if s == "\x1b[3~" {
        // delete key — parse.keypress.ts:472-477 (unreachable via the
        // fn-key branch for this exact input; ported order-exact)
        key.name = "delete".into();
        key.meta = false;
        key.code = Some("[3~".into());
    }

    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> ParsedKey {
        parse_keypress(s, ParseKeypressOptions::default()).unwrap()
    }

    #[test]
    fn basic_letters_and_numbers() {
        // parse.keypress.test.ts:5-46
        let a = k("a");
        assert_eq!(a.name, "a");
        assert!(!a.ctrl && !a.meta && !a.shift && !a.option && !a.number);
        assert_eq!(a.sequence, "a");
        assert_eq!(a.raw, "a");
        assert_eq!(a.event_type, KeyEventType::Press);
        assert_eq!(a.source, KeySource::Raw);

        let upper = k("A");
        assert_eq!(upper.name, "a");
        assert!(upper.shift);
        assert_eq!(upper.sequence, "A");

        let one = k("1");
        assert_eq!(one.name, "1");
        assert!(one.number);
    }

    #[test]
    fn special_keys() {
        // parse.keypress.test.ts:48-152
        assert_eq!(k("\r").name, "return");
        assert_eq!(k("\n").name, "linefeed");
        let meta_ret = k("\x1b\r");
        assert_eq!(meta_ret.name, "return");
        assert!(meta_ret.meta);
        let meta_lf = k("\x1b\n");
        assert_eq!(meta_lf.name, "linefeed");
        assert!(meta_lf.meta);
        assert_eq!(k("\t").name, "tab");
        assert_eq!(k("\x08").name, "backspace");
        assert_eq!(k("\x1b").name, "escape");
        assert_eq!(k(" ").name, "space");
        let meta_space = k("\x1b ");
        assert_eq!(meta_space.name, "space");
        assert!(meta_space.meta);
        assert!(!meta_space.option);
        let meta_bs = k("\x1b\x7f");
        assert_eq!(meta_bs.name, "backspace");
        assert!(meta_bs.meta);
        let dbl_esc = k("\x1b\x1b");
        assert_eq!(dbl_esc.name, "escape");
        assert!(dbl_esc.meta);
    }

    #[test]
    fn ctrl_letters_and_control_bytes() {
        // parse.keypress.test.ts:154-288
        let ca = k("\x01");
        assert_eq!(ca.name, "a");
        assert!(ca.ctrl);
        assert_eq!(k("\x1a").name, "z");
        assert_eq!(k("\x00").name, "space");
        assert!(k("\x00").ctrl);
        assert_eq!(k("\x1c").name, "\\");
        assert_eq!(k("\x1d").name, "]");
        assert_eq!(k("\x1e").name, "^");
        assert_eq!(k("\x1f").name, "_");
        let mc = k("\x1b\x1c");
        assert_eq!(mc.name, "\\");
        assert!(mc.ctrl && mc.meta);
    }

    #[test]
    fn meta_chars_and_uppercase_arrows() {
        // parse.keypress.test.ts:350-380, 1874-1929
        let meta_x = k("\x1bx");
        assert_eq!(meta_x.name, "x");
        assert!(meta_x.meta);
        let meta_upper = k("\x1bX");
        assert_eq!(meta_upper.name, "X");
        assert!(meta_upper.shift && meta_upper.meta);
        let f = k("\x1bF");
        assert_eq!(f.name, "right");
        assert!(f.meta);
        let b = k("\x1bB");
        assert_eq!(b.name, "left");
        assert!(b.meta);
    }

    #[test]
    fn function_and_arrow_keys() {
        // parse.keypress.test.ts:382-497
        assert_eq!(k("\x1bOP").name, "f1");
        assert_eq!(k("\x1bOQ").name, "f2");
        assert_eq!(k("\x1b[15~").name, "f5");
        assert_eq!(k("\x1b[11~").name, "f1");
        assert_eq!(k("\x1b[[A").name, "f1");
        assert_eq!(k("\x1b[A").name, "up");
        assert_eq!(k("\x1b[B").name, "down");
        assert_eq!(k("\x1b[C").name, "right");
        assert_eq!(k("\x1b[D").name, "left");
        assert_eq!(k("\x1bOA").name, "up");
        assert_eq!(k("\x1b[H").name, "home");
        assert_eq!(k("\x1b[F").name, "end");
        assert_eq!(k("\x1b[2~").name, "insert");
        assert_eq!(k("\x1b[3~").name, "delete");
        assert_eq!(k("\x1b[3~").code.as_deref(), Some("[3~"));
        assert_eq!(k("\x1b[5~").name, "pageup");
        assert_eq!(k("\x1b[6~").name, "pagedown");
        assert_eq!(k("\x1b[1~").name, "home");
        assert_eq!(k("\x1b[4~").name, "end");
    }

    #[test]
    fn modifier_combinations() {
        // parse.keypress.test.ts:564-720, 1006-1072
        let shift_up = k("\x1b[1;2A");
        assert_eq!(shift_up.name, "up");
        assert!(shift_up.shift);
        assert_eq!(shift_up.code.as_deref(), Some("[A"));
        let ctrl_up = k("\x1b[1;5A");
        assert_eq!(ctrl_up.name, "up");
        assert!(ctrl_up.ctrl);
        let shift_del = k("\x1b[3;2~");
        assert_eq!(shift_del.name, "delete");
        assert!(shift_del.shift);
        assert_eq!(shift_del.code.as_deref(), Some("[3~"));
        let ctrl_del = k("\x1b[3;5~");
        assert_eq!(ctrl_del.name, "delete");
        assert!(ctrl_del.ctrl);
        // rxvt style: ESC [ a — shift + up (parse.keypress.test.ts:1006)
        let rxvt = k("\x1b[a");
        assert_eq!(rxvt.name, "up");
        assert!(rxvt.shift);
        let rxvt_ctrl = k("\x1bOa");
        assert_eq!(rxvt_ctrl.name, "up");
        assert!(rxvt_ctrl.ctrl);
        // shift+tab
        let stab = k("\x1b[Z");
        assert_eq!(stab.name, "tab");
        assert!(stab.shift);
        // double-ESC fn key keeps option+meta (parse.keypress.test.ts:1931)
        let dbl = k("\x1b\x1b[A");
        assert_eq!(dbl.name, "up");
        assert!(dbl.meta && dbl.option);
    }

    #[test]
    fn modify_other_keys() {
        // parse.keypress.test.ts:740-798, 1634-1873
        let shift_enter = k("\x1b[27;2;13~");
        assert_eq!(shift_enter.name, "return");
        assert!(shift_enter.shift);
        assert!(!shift_enter.ctrl);
        let ctrl_enter = k("\x1b[27;5;13~");
        assert_eq!(ctrl_enter.name, "return");
        assert!(ctrl_enter.ctrl);
        let ctrl_esc = k("\x1b[27;5;27~");
        assert_eq!(ctrl_esc.name, "escape");
        let ctrl_tab = k("\x1b[27;5;9~");
        assert_eq!(ctrl_tab.name, "tab");
        let shift_space = k("\x1b[27;2;32~");
        assert_eq!(shift_space.name, "space");
        assert!(shift_space.shift);
        let shift_bs = k("\x1b[27;2;127~");
        assert_eq!(shift_bs.name, "backspace");
        // digits keep number=true
        let five = k("\x1b[27;3;53~");
        assert_eq!(five.name, "5");
        assert!(five.number);
        assert_eq!(five.sequence, "5");
        // 8 also maps to backspace
        let bs8 = k("\x1b[27;2;8~");
        assert_eq!(bs8.name, "backspace");
    }

    #[test]
    fn filters() {
        // parse.keypress.test.ts:1371-1473
        assert!(parse_keypress("\x1b[<0;10;20M", ParseKeypressOptions::default()).is_none());
        assert!(parse_keypress("\x1b[<0;10;20m", ParseKeypressOptions::default()).is_none());
        assert!(parse_keypress("[<0;10;20M", ParseKeypressOptions::default()).is_none());
        assert!(parse_keypress("\x1b[<0;10;", ParseKeypressOptions::default()).is_none());
        assert!(parse_keypress("[<32;", ParseKeypressOptions::default()).is_none());
        assert!(parse_keypress("\x1b[M !!", ParseKeypressOptions::default()).is_none());
        assert!(parse_keypress("\x1b[4;10;20t", ParseKeypressOptions::default()).is_none());
        assert!(parse_keypress("\x1b[8;10;20t", ParseKeypressOptions::default()).is_none());
        assert!(parse_keypress("\x1b[10;20R", ParseKeypressOptions::default()).is_none());
        assert!(parse_keypress("\x1b[?62;c", ParseKeypressOptions::default()).is_none());
        assert!(parse_keypress("\x1b[?1;2$y", ParseKeypressOptions::default()).is_none());
        assert!(parse_keypress("\x1b[I", ParseKeypressOptions::default()).is_none());
        assert!(parse_keypress("\x1b[O", ParseKeypressOptions::default()).is_none());
        assert!(parse_keypress("\x1b]0;title\x07", ParseKeypressOptions::default()).is_none());
        assert!(
            parse_keypress("\x1b]11;rgb:1/1/1\x1b\\", ParseKeypressOptions::default()).is_none()
        );
        assert!(parse_keypress("\x1b[200~", ParseKeypressOptions::default()).is_none());
        assert!(parse_keypress("\x1b[201~", ParseKeypressOptions::default()).is_none());
        // similar-looking valid keys are NOT filtered
        assert!(parse_keypress("\x1b[3~", ParseKeypressOptions::default()).is_some());
        assert!(parse_keypress("\x1b[200", ParseKeypressOptions::default()).is_some());
        assert!(parse_keypress("\x1b[<", ParseKeypressOptions::default()).is_none());
        // partial SGR
    }

    #[test]
    fn buffer_high_byte_meta() {
        // parse.keypress.test.ts:890-921
        let alt_a = parse_keypress_bytes(&[0xe1], ParseKeypressOptions::default()).unwrap();
        assert_eq!(alt_a.name, "a");
        assert!(alt_a.meta);
        assert_eq!(alt_a.sequence, "\x1ba");
        // multi-byte buffer decodes as UTF-8
        let euro = parse_keypress_bytes("é".as_bytes(), ParseKeypressOptions::default()).unwrap();
        assert_eq!(euro.name, "é");
    }

    #[test]
    fn ss3_application_keypad() {
        // parse.keypress.test.ts:1959-2010
        let zero = k("\x1bOp");
        assert_eq!(zero.name, "0");
        assert!(zero.number);
        assert_eq!(zero.sequence, "0");
        let plus = k("\x1bOk");
        assert_eq!(plus.name, "+");
        assert_eq!(plus.sequence, "+");
        assert!(!plus.number);
        let enter = k("\x1bOM");
        assert_eq!(enter.name, "return");
        // OM has no printable override — the sequence stays raw
        assert_eq!(enter.sequence, "\x1bOM");
        let eq = k("\x1bOX");
        assert_eq!(eq.name, "=");
    }

    #[test]
    fn preserves_non_bmp_characters() {
        // parse.keypress.test.ts:1947-1957
        let emoji = k("😀");
        assert_eq!(emoji.name, "😀");
        assert_eq!(emoji.sequence, "😀");
    }

    #[test]
    fn empty_input_is_blank_key() {
        // parse.keypress.test.ts:922-948
        let e = parse_keypress("", ParseKeypressOptions::default()).unwrap();
        assert_eq!(e.name, "");
        assert_eq!(e.sequence, "");
    }

    #[test]
    fn non_alphanumeric_keys_export() {
        // parse.keypress.test.ts:1074-1101
        let keys = non_alphanumeric_keys();
        assert!(keys.contains(&"return"));
        assert!(keys.contains(&"up"));
        assert!(keys.contains(&"f12"));
        assert!(keys.contains(&"backspace"));
        assert!(keys.contains(&"="));
        assert_eq!(keys.last(), Some(&"backspace"));
        // Object.values order: first entries are the ESC O letters
        assert_eq!(keys[0], "f1");
    }

    #[test]
    fn terminal_named_single_stroke_keys_dedups() {
        let keys = terminal_named_single_stroke_keys();
        let mut seen = std::collections::HashSet::new();
        for k in &keys {
            assert!(seen.insert(*k), "duplicate {k}");
        }
        assert!(keys.contains(&"return"));
        assert!(keys.contains(&"kpenter"));
        assert!(keys.contains(&"f35"));
    }
}
