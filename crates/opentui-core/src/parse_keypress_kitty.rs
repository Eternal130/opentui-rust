//! `parseKittyKeyboard` — the Kitty keyboard protocol parser
//! (`packages/core/src/lib/parse.keypress-kitty.ts`), spec
//! <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>.
//!
//! The [`KITTY_KEY_MAP`] table, `printableKeypadText`, the modifier
//! decode ([`from_kitty_mods`]) and the special-key/event-type parsing
//! are order- and value-exact ports. JS `parseInt` semantics
//! (leading-digit parse, NaN fallback) are reproduced.

use crate::parse_keypress::{KeyEventType, KeySource, ParsedKey};

/// `kittyKeyMap` (parse.keypress-kitty.ts:6-137) — exact.
const KITTY_KEY_MAP: &[(u32, &str)] = &[
    // Standard keys
    (27, "escape"),
    (9, "tab"),
    (13, "return"),
    (127, "backspace"),
    // Arrow keys
    (57344, "escape"),
    (57345, "return"),
    (57346, "tab"),
    (57347, "backspace"),
    (57348, "insert"),
    (57349, "delete"),
    (57350, "left"),
    (57351, "right"),
    (57352, "up"),
    (57353, "down"),
    (57354, "pageup"),
    (57355, "pagedown"),
    (57356, "home"),
    (57357, "end"),
    (57358, "capslock"),
    (57359, "scrolllock"),
    (57360, "numlock"),
    (57361, "printscreen"),
    (57362, "pause"),
    (57363, "menu"),
    // Function keys
    (57364, "f1"),
    (57365, "f2"),
    (57366, "f3"),
    (57367, "f4"),
    (57368, "f5"),
    (57369, "f6"),
    (57370, "f7"),
    (57371, "f8"),
    (57372, "f9"),
    (57373, "f10"),
    (57374, "f11"),
    (57375, "f12"),
    (57376, "f13"),
    (57377, "f14"),
    (57378, "f15"),
    (57379, "f16"),
    (57380, "f17"),
    (57381, "f18"),
    (57382, "f19"),
    (57383, "f20"),
    (57384, "f21"),
    (57385, "f22"),
    (57386, "f23"),
    (57387, "f24"),
    (57388, "f25"),
    (57389, "f26"),
    (57390, "f27"),
    (57391, "f28"),
    (57392, "f29"),
    (57393, "f30"),
    (57394, "f31"),
    (57395, "f32"),
    (57396, "f33"),
    (57397, "f34"),
    (57398, "f35"),
    // Keypad
    (57399, "kp0"),
    (57400, "kp1"),
    (57401, "kp2"),
    (57402, "kp3"),
    (57403, "kp4"),
    (57404, "kp5"),
    (57405, "kp6"),
    (57406, "kp7"),
    (57407, "kp8"),
    (57408, "kp9"),
    (57409, "kpdecimal"),
    (57410, "kpdivide"),
    (57411, "kpmultiply"),
    (57412, "kpminus"),
    (57413, "kpplus"),
    (57414, "kpenter"),
    (57415, "kpequal"),
    (57416, "kpseparator"),
    (57417, "kpleft"),
    (57418, "kpright"),
    (57419, "kpup"),
    (57420, "kpdown"),
    (57421, "kppageup"),
    (57422, "kppagedown"),
    (57423, "kphome"),
    (57424, "kpend"),
    (57425, "kpinsert"),
    (57426, "kpdelete"),
    (57427, "clear"),
    // Media keys
    (57428, "mediaplay"),
    (57429, "mediapause"),
    (57430, "mediaplaypause"),
    (57431, "mediareverse"),
    (57432, "mediastop"),
    (57433, "mediafastforward"),
    (57434, "mediarewind"),
    (57435, "medianext"),
    (57436, "mediaprev"),
    (57437, "mediarecord"),
    // Volume keys
    (57438, "volumedown"),
    (57439, "volumeup"),
    (57440, "mute"),
    // Modifiers
    (57441, "leftshift"),
    (57442, "leftctrl"),
    (57443, "leftalt"),
    (57444, "leftsuper"),
    (57445, "lefthyper"),
    (57446, "leftmeta"),
    (57447, "rightshift"),
    (57448, "rightctrl"),
    (57449, "rightalt"),
    (57450, "rightsuper"),
    (57451, "righthyper"),
    (57452, "rightmeta"),
    // Special
    (57453, "iso_level3_shift"),
    (57454, "iso_level5_shift"),
];

fn kitty_key_map(codepoint: u32) -> Option<&'static str> {
    KITTY_KEY_MAP
        .iter()
        .find(|(cp, _)| *cp == codepoint)
        .map(|(_, name)| *name)
}

/// `kittyNamedSingleStrokeKeys` (parse.keypress-kitty.ts:139) — the
/// `Set`-deduped value list, first-occurrence order.
pub fn kitty_named_single_stroke_keys() -> Vec<&'static str> {
    let mut seen: Vec<&'static str> = Vec::new();
    for (_, name) in KITTY_KEY_MAP {
        if !seen.contains(name) {
            seen.push(name);
        }
    }
    seen
}

/// `printableKeypadText` (parse.keypress-kitty.ts:141-159).
const PRINTABLE_KEYPAD_TEXT: &[(&str, &str)] = &[
    ("kp0", "0"),
    ("kp1", "1"),
    ("kp2", "2"),
    ("kp3", "3"),
    ("kp4", "4"),
    ("kp5", "5"),
    ("kp6", "6"),
    ("kp7", "7"),
    ("kp8", "8"),
    ("kp9", "9"),
    ("kpdecimal", "."),
    ("kpdivide", "/"),
    ("kpmultiply", "*"),
    ("kpminus", "-"),
    ("kpplus", "+"),
    ("kpequal", "="),
    ("kpseparator", ","),
];

fn printable_kitty_key_text(name: &str) -> Option<&'static str> {
    PRINTABLE_KEYPAD_TEXT
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| *v)
}

/// `fromKittyMods` (parse.keypress-kitty.ts:165-185).
#[derive(Debug, Clone, Copy, Default)]
struct KittyMods {
    shift: bool,
    alt: bool,
    ctrl: bool,
    super_key: bool,
    hyper: bool,
    meta: bool,
    caps_lock: bool,
    num_lock: bool,
}

fn from_kitty_mods(m: u32) -> KittyMods {
    KittyMods {
        shift: m & 1 != 0,
        alt: m & 2 != 0,
        ctrl: m & 4 != 0,
        super_key: m & 8 != 0,
        hyper: m & 16 != 0,
        meta: m & 32 != 0,
        caps_lock: m & 64 != 0,
        num_lock: m & 128 != 0,
    }
}

/// `functionalKeyMap` (parse.keypress-kitty.ts:188-199).
const FUNCTIONAL_KEY_MAP: &[(char, &str)] = &[
    ('A', "up"),
    ('B', "down"),
    ('C', "right"),
    ('D', "left"),
    ('H', "home"),
    ('F', "end"),
    ('E', "clear"),
    ('P', "f1"),
    ('Q', "f2"),
    ('S', "f4"),
];

fn functional_key_map(letter: char) -> Option<&'static str> {
    FUNCTIONAL_KEY_MAP
        .iter()
        .find(|(l, _)| *l == letter)
        .map(|(_, name)| *name)
}

/// `tildeKeyMap` (parse.keypress-kitty.ts:202-225).
const TILDE_KEY_MAP: &[(&str, &str)] = &[
    ("1", "home"),
    ("2", "insert"),
    ("3", "delete"),
    ("4", "end"),
    ("5", "pageup"),
    ("6", "pagedown"),
    ("7", "home"), // rxvt
    ("8", "end"),  // rxvt
    ("11", "f1"),
    ("12", "f2"),
    ("13", "f3"),
    ("14", "f4"),
    ("15", "f5"),
    ("17", "f6"),
    ("18", "f7"),
    ("19", "f8"),
    ("20", "f9"),
    ("21", "f10"),
    ("23", "f11"),
    ("24", "f12"),
    ("29", "menu"),
    ("57427", "clear"),
];

fn tilde_key_map(num: &str) -> Option<&'static str> {
    TILDE_KEY_MAP
        .iter()
        .find(|(n, _)| *n == num)
        .map(|(_, name)| *name)
}

/// JS `parseInt(str, 10)`: parses the leading decimal digits, NaN
/// otherwise (including empty string).
fn js_parse_int(s: &str) -> Option<u32> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    // parseInt is 32-bit-ish in practice for these inputs; saturate like
    // a double parse that then feeds bit math (upstream `modifier & 4`).
    digits
        .parse::<u64>()
        .ok()
        .map(|v| v.min(u32::MAX as u64) as u32)
}

/// `parseKittySpecialKey` (parse.keypress-kitty.ts:233-302):
/// `^\x1b\[(\d+);(\d+):(\d+)([A-Z~])$` — functional and tilde keys
/// with event types.
fn parse_kitty_special_key(sequence: &str) -> Option<ParsedKey> {
    let rest = sequence.strip_prefix("\x1b[")?;
    let last = rest.chars().last()?;
    if !last.is_ascii_uppercase() && last != '~' {
        return None;
    }
    let params = &rest[..rest.len() - last.len_utf8()];
    let (key_num, modifier_str, event_type_str) = split_special_params(params)?;

    // Determine key name based on terminator (lines 245-254)
    let key_name = if last == '~' {
        tilde_key_map(&key_num)?
    } else {
        // Functional key: must have "1" as first param
        if key_num != "1" {
            return None;
        }
        functional_key_map(last)?
    };

    let mut key = ParsedKey {
        name: key_name.to_owned(),
        ctrl: false,
        meta: false,
        shift: false,
        option: false,
        number: false,
        sequence: sequence.to_owned(),
        raw: sequence.to_owned(),
        event_type: KeyEventType::Press,
        source: KeySource::Kitty,
        code: None,
        super_key: Some(false),
        hyper: Some(false),
        caps_lock: Some(false),
        num_lock: Some(false),
        base_code: None,
        repeated: None,
    };

    // Parse modifiers (lines 275-289)
    if !modifier_str.is_empty() {
        if let Some(mask) = js_parse_int(&modifier_str) {
            if mask > 1 {
                let mods = from_kitty_mods(mask - 1);
                key.shift = mods.shift;
                key.ctrl = mods.ctrl;
                key.meta = mods.alt || mods.meta;
                key.option = mods.alt;
                key.super_key = Some(mods.super_key);
                key.hyper = Some(mods.hyper);
                key.caps_lock = Some(mods.caps_lock);
                key.num_lock = Some(mods.num_lock);
            }
        }
    }

    // Parse event type: 1 = press, 2 = repeat, 3 = release (lines 291-299)
    key.event_type = match &event_type_str[..] {
        "2" => {
            key.repeated = Some(true);
            KeyEventType::Press
        }
        "3" => KeyEventType::Release,
        // "1", "", and unknown values default to press
        _ => KeyEventType::Press,
    };

    Some(key)
}

/// Splits `num;mod:event` into its three parts — the capture groups of
/// the special-key regex.
fn split_special_params(params: &str) -> Option<(String, String, String)> {
    let (num, rest) = params.split_once(';')?;
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (modifier, event) = rest.split_once(':')?;
    if modifier.is_empty() || !modifier.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if event.is_empty() || !event.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((num.to_owned(), modifier.to_owned(), event.to_owned()))
}

/// `parseKittyKeyboard` (parse.keypress-kitty.ts:304-469): the CSI-u
/// form `CSI unicode-key-code[:shifted[:base]] [; modifiers[:event]]
/// [; text-as-codepoints] u`.
///
/// Divergence: JS `String.fromCodePoint` throws on surrogate
/// codepoints (0xD800-0xDFFF); the port treats them as invalid and
/// returns `None` (falling back to legacy parsing) instead of
/// propagating a `RangeError`.
#[must_use]
pub fn parse_kitty_keyboard(sequence: &str) -> Option<ParsedKey> {
    // Try special key format first (line 305-307)
    if let Some(special) = parse_kitty_special_key(sequence) {
        return Some(special);
    }

    // CSI ... u — `^\x1b\[([^\x1b]+)u$` (line 310)
    let rest = sequence.strip_prefix("\x1b[")?;
    let params = rest.strip_suffix('u')?;
    if params.is_empty() {
        return None;
    }

    let fields: Vec<&str> = params.split(';').collect();
    if fields.is_empty() {
        return None;
    }

    let mut key = ParsedKey {
        name: String::new(),
        ctrl: false,
        meta: false,
        shift: false,
        option: false,
        number: false,
        sequence: sequence.to_owned(),
        raw: sequence.to_owned(),
        event_type: KeyEventType::Press,
        source: KeySource::Kitty,
        code: None,
        super_key: Some(false),
        hyper: Some(false),
        caps_lock: Some(false),
        num_lock: Some(false),
        base_code: None,
        repeated: None,
    };

    let mut text = String::new();

    // Field 1: unicode-key-code:shifted_codepoint:base_layout_codepoint
    // (lines 339-387)
    let field1: Vec<&str> = fields[0].split(':').collect();
    let codepoint_str = field1.first().copied().unwrap_or("");
    if codepoint_str.is_empty() {
        return None;
    }
    let codepoint = js_parse_int(codepoint_str)?;

    let mut shifted_codepoint: Option<u32> = None;
    let mut base_codepoint: Option<u32> = None;

    if let Some(shifted) = field1.get(1) {
        if let Some(v) = js_parse_int(shifted) {
            if v > 0 && v <= 0x10ffff {
                shifted_codepoint = Some(v);
            }
        }
    }
    if let Some(base) = field1.get(2) {
        if let Some(v) = js_parse_int(base) {
            if v > 0 && v <= 0x10ffff {
                base_codepoint = Some(v);
            }
        }
    }

    let known_key = kitty_key_map(codepoint);
    if let Some(known) = known_key {
        key.name = known.to_owned();
        key.code = Some(format!("[{codepoint}u"));
    } else if codepoint == 0 {
        key.name = String::new();
    } else if codepoint > 0 && codepoint <= 0x10ffff {
        // It's a Unicode character
        let ch = char::from_u32(codepoint)?; // None on surrogates (see doc)
        key.name = if ch == ' ' {
            "space".to_owned()
        } else {
            ch.to_string()
        };
        if let Some(base) = base_codepoint {
            key.base_code = Some(base);
        }
    } else {
        return None; // Invalid codepoint
    }

    // Field 2: modifier_mask:event_type (lines 389-421)
    if let Some(field2) = fields.get(1) {
        let mut parts = field2.split(':');
        let modifier_str = parts.next().unwrap_or("");
        let event_type_str = parts.next().unwrap_or("");

        if !modifier_str.is_empty() {
            if let Some(mask) = js_parse_int(modifier_str) {
                if mask > 1 {
                    let mods = from_kitty_mods(mask - 1); // Kitty modifiers start from 1
                    key.shift = mods.shift;
                    key.ctrl = mods.ctrl;
                    key.meta = mods.alt || mods.meta;
                    key.option = mods.alt;
                    key.super_key = Some(mods.super_key);
                    key.hyper = Some(mods.hyper);
                    key.caps_lock = Some(mods.caps_lock);
                    key.num_lock = Some(mods.num_lock);
                }
            }
        }

        // Event type: 1 = press (default), 2 = repeat, 3 = release
        key.event_type = match event_type_str {
            "2" => {
                key.repeated = Some(true);
                KeyEventType::Press
            }
            "3" => KeyEventType::Release,
            _ => KeyEventType::Press,
        };
    }

    // Field 3: text_as_codepoint[:text_as_codepoint] (lines 423-432)
    if let Some(field3) = fields.get(2) {
        for cp_str in field3.split(':') {
            if let Some(cp) = js_parse_int(cp_str) {
                if cp > 0 && cp <= 0x10ffff {
                    if let Some(ch) = char::from_u32(cp) {
                        text.push(ch);
                    }
                }
            }
        }
    }

    if text.is_empty() {
        text = printable_kitty_key_text(&key.name).map_or_else(String::new, str::to_owned);
    }

    // Handle text generation for printable characters (lines 438-456)
    if text.is_empty() {
        let is_printable = !key.name.is_empty() && known_key.is_none();
        if is_printable {
            if codepoint == 32 {
                text = " ".to_owned();
            } else if key.shift && shifted_codepoint.is_some() {
                text = char::from_u32(shifted_codepoint.unwrap())?.to_string();
            } else if key.shift && key.name.chars().count() == 1 {
                // Uppercase the character (works for Unicode incl. Cyrillic)
                text = key.name.to_uppercase();
            } else {
                text = key.name.clone();
            }
        }
    }

    if !text.is_empty() {
        if codepoint == 0 {
            key.name = text.clone();
        }
        key.sequence = text.clone();
    }

    if codepoint == 0 && text.is_empty() {
        return None;
    }

    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kitty(s: &str) -> Option<ParsedKey> {
        parse_kitty_keyboard(s)
    }

    #[test]
    fn basic_keys() {
        // parse.keypress-kitty.test.ts:11-64
        let a = kitty("\x1b[97u").unwrap();
        assert_eq!(a.name, "a");
        assert_eq!(a.sequence, "a");
        assert_eq!(a.source, KeySource::Kitty);
        let shift_a = kitty("\x1b[97:65;2u").unwrap();
        assert_eq!(shift_a.name, "a");
        assert!(shift_a.shift);
        assert_eq!(shift_a.sequence, "A");
        let ctrl_a = kitty("\x1b[97;5u").unwrap();
        assert_eq!(ctrl_a.name, "a");
        assert!(ctrl_a.ctrl);
        assert!(!ctrl_a.meta);
        let alt_a = kitty("\x1b[97;3u").unwrap();
        assert_eq!(alt_a.name, "a");
        assert!(alt_a.meta && alt_a.option);
        let f1 = kitty("\x1b[57364u").unwrap();
        assert_eq!(f1.name, "f1");
        let up = kitty("\x1b[57352u").unwrap();
        assert_eq!(up.name, "up");
        let shift_space = kitty("\x1b[32:65;2u").unwrap();
        assert_eq!(shift_space.name, "space");
        assert_eq!(shift_space.sequence, " ");
        let ctrl_space = kitty("\x1b[32;5u").unwrap();
        assert_eq!(ctrl_space.name, "space");
    }

    #[test]
    fn event_types() {
        // parse.keypress-kitty.test.ts:81-124, 472-495
        let press = kitty("\x1b[97;1:1u").unwrap();
        assert_eq!(press.event_type, KeyEventType::Press);
        assert!(press.repeated.is_none());
        let repeat = kitty("\x1b[97;1:2u").unwrap();
        assert_eq!(repeat.event_type, KeyEventType::Press);
        assert_eq!(repeat.repeated, Some(true));
        let release = kitty("\x1b[97;1:3u").unwrap();
        assert_eq!(release.event_type, KeyEventType::Release);
        // unknown event type defaults to press
        let unknown = kitty("\x1b[97;1:9u").unwrap();
        assert_eq!(unknown.event_type, KeyEventType::Press);
        // empty event type defaults to press
        let empty = kitty("\x1b[97;1:u").unwrap();
        assert_eq!(empty.event_type, KeyEventType::Press);
        // combined mods + release
        let combo = kitty("\x1b[97;5:3u").unwrap();
        assert_eq!(combo.event_type, KeyEventType::Release);
        assert!(combo.ctrl);
    }

    #[test]
    fn text_field() {
        // parse.keypress-kitty.test.ts:125-130
        let with_text = kitty("\x1b[97;;97u").unwrap();
        assert_eq!(with_text.name, "a");
        assert_eq!(with_text.sequence, "a");
    }

    #[test]
    fn modifier_flags() {
        // parse.keypress-kitty.test.ts:131-163
        let cs = kitty("\x1b[97;6u").unwrap();
        assert!(cs.ctrl && cs.shift);
        assert!(!cs.meta);
        let asx = kitty("\x1b[97;4u").unwrap();
        assert!(asx.meta && asx.shift && asx.option);
        let sup = kitty("\x1b[97;9u").unwrap();
        assert_eq!(sup.super_key, Some(true));
        let hyp = kitty("\x1b[97;17u").unwrap();
        assert_eq!(hyp.hyper, Some(true));
    }

    #[test]
    fn shifted_and_base_codepoints() {
        // parse.keypress-kitty.test.ts:164-189
        let shifted = kitty("\x1b[97:65;2u").unwrap();
        assert_eq!(shifted.sequence, "A");
        // base layout codepoint preserved
        let base = kitty("\x1b[97:97:99u").unwrap();
        assert_eq!(base.name, "a");
        assert_eq!(base.base_code, Some(99));
        // AZERTY mapping keeps name from primary codepoint
        let az = kitty("\x1b[97:97:122u").unwrap();
        assert_eq!(az.name, "a");
        assert_eq!(az.base_code, Some(122));
    }

    #[test]
    fn lock_keys() {
        // parse.keypress-kitty.test.ts:190-220
        let caps = kitty("\x1b[97;65u").unwrap();
        assert_eq!(caps.caps_lock, Some(true));
        let num = kitty("\x1b[97;129u").unwrap();
        assert_eq!(num.num_lock, Some(true));
    }

    #[test]
    fn unicode_and_invalid() {
        // parse.keypress-kitty.test.ts:221-245
        let e_acute = kitty("\x1b[233u").unwrap();
        assert_eq!(e_acute.name, "é");
        let emoji = kitty("\x1b[128512u").unwrap();
        assert_eq!(emoji.name, "😀");
        assert!(kitty("\x1b[1114112u").is_none());
        // surrogate → None (upstream throws; documented divergence)
        assert!(kitty("\x1b[55296u").is_none());
        // codepoint 0 with no text → None (line 465-467)
        assert!(kitty("\x1b[0u").is_none());
    }

    #[test]
    fn keypad_keys() {
        // parse.keypress-kitty.test.ts:246-296
        let cases = [
            ("\x1b[57399u", "kp0", "0"),
            ("\x1b[57408u", "kp9", "9"),
            ("\x1b[57409u", "kpdecimal", "."),
            ("\x1b[57410u", "kpdivide", "/"),
            ("\x1b[57411u", "kpmultiply", "*"),
            ("\x1b[57412u", "kpminus", "-"),
            ("\x1b[57413u", "kpplus", "+"),
            ("\x1b[57415u", "kpequal", "="),
            ("\x1b[57416u", "kpseparator", ","),
        ];
        for (seq, name, text) in cases {
            let k = kitty(seq).unwrap();
            assert_eq!(k.name, name);
            assert_eq!(k.sequence, text);
        }
        // non-printable keypad keys keep the sequence
        let enter = kitty("\x1b[57414u").unwrap();
        assert_eq!(enter.name, "kpenter");
        assert_eq!(enter.sequence, "\x1b[57414u");
    }

    #[test]
    fn media_and_modifier_keys() {
        // parse.keypress-kitty.test.ts:297-318
        let play = kitty("\x1b[57428u").unwrap();
        assert_eq!(play.name, "mediaplay");
        let lshift = kitty("\x1b[57441u").unwrap();
        assert_eq!(lshift.name, "leftshift");
        let rmeta = kitty("\x1b[57452u").unwrap();
        assert_eq!(rmeta.name, "rightmeta");
    }

    #[test]
    fn special_keys_with_event_types() {
        // parse.keypress-kitty.test.ts:319-457
        let up_press = kitty("\x1b[1;1:1A").unwrap();
        assert_eq!(up_press.name, "up");
        assert_eq!(up_press.event_type, KeyEventType::Press);
        assert_eq!(up_press.source, KeySource::Kitty);
        let pgup_repeat = kitty("\x1b[5;1:2~").unwrap();
        assert_eq!(pgup_repeat.name, "pageup");
        assert_eq!(pgup_repeat.repeated, Some(true));
        let del_release = kitty("\x1b[3;5:3~").unwrap();
        assert_eq!(del_release.name, "delete");
        assert_eq!(del_release.event_type, KeyEventType::Release);
        assert!(del_release.ctrl);
        let f1_press = kitty("\x1b[1;1:1P").unwrap();
        assert_eq!(f1_press.name, "f1");
        let home_ev = kitty("\x1b[1;1:1H").unwrap();
        assert_eq!(home_ev.name, "home");
        let tilde_f5 = kitty("\x1b[15;1:1~").unwrap();
        assert_eq!(tilde_f5.name, "f5");
        // modifier forms
        let shift_up = kitty("\x1b[1;2:1A").unwrap();
        assert!(shift_up.shift);
        let ctrl_down = kitty("\x1b[1;5:3B").unwrap();
        assert!(ctrl_down.ctrl);
        assert_eq!(ctrl_down.event_type, KeyEventType::Release);
    }

    #[test]
    fn shift_without_shifted_codepoint_uppercases() {
        // parse.keypress-kitty.test.ts:696-712
        let shift_c = kitty("\x1b[99;2u").unwrap();
        assert_eq!(shift_c.name, "c");
        assert_eq!(shift_c.sequence, "C");
        // Cyrillic uppercases too
        let cyr = kitty("\x1b[1074;2u").unwrap();
        assert_eq!(cyr.name, "в");
        assert_eq!(cyr.sequence, "В");
    }

    #[test]
    fn non_kitty_sequences_return_none() {
        // progressive enhancement fallback (parse.keypress-kitty.test.ts:497-505)
        assert!(kitty("\x1b[1;2A").is_none());
        assert!(kitty("a").is_none());
        assert!(kitty("\x1b[A").is_none());
        assert!(kitty("").is_none());
    }
}
