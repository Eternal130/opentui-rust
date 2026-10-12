//! `MouseParser` — `packages/core/src/lib/parse.mouse.ts` (exact port).
//!
//! SGR (`ESC[<btn;x;yM/m`) and X10 basic (`ESC[M` + 3 payload bytes)
//! decoding, including the pressed-button tracking that classifies
//! motion as `drag` vs `move`. Raw payload bytes are handled as latin1
//! (byte-preserving) like upstream's `decodeInput`
//! (parse.mouse.ts:36-41).

/// `MouseEventType` (parse.mouse.ts:1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEventType {
    Down,
    Up,
    Move,
    Drag,
    DragEnd,
    Drop,
    Over,
    Out,
    Scroll,
}

impl MouseEventType {
    /// The wire name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            MouseEventType::Down => "down",
            MouseEventType::Up => "up",
            MouseEventType::Move => "move",
            MouseEventType::Drag => "drag",
            MouseEventType::DragEnd => "drag-end",
            MouseEventType::Drop => "drop",
            MouseEventType::Over => "over",
            MouseEventType::Out => "out",
            MouseEventType::Scroll => "scroll",
        }
    }
}

/// `ScrollInfo` (parse.mouse.ts:3-6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollInfo {
    pub direction: ScrollDirection,
    pub delta: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

impl ScrollDirection {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ScrollDirection::Up => "up",
            ScrollDirection::Down => "down",
            ScrollDirection::Left => "left",
            ScrollDirection::Right => "right",
        }
    }
}

/// `RawMouseEvent` (parse.mouse.ts:8-15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawMouseEvent {
    pub event_type: MouseEventType,
    pub button: i32,
    pub x: i32,
    pub y: i32,
    pub modifiers: MouseModifiers,
    pub scroll: Option<ScrollInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MouseModifiers {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

struct ParsedMouseSequence {
    event: RawMouseEvent,
    consumed: usize,
}

/// `SCROLL_DIRECTIONS` (parse.mouse.ts:25-30).
fn scroll_direction(button: u32) -> Option<ScrollDirection> {
    match button {
        0 => Some(ScrollDirection::Up),
        1 => Some(ScrollDirection::Down),
        2 => Some(ScrollDirection::Left),
        3 => Some(ScrollDirection::Right),
        _ => None,
    }
}

/// `MouseParser` (parse.mouse.ts:22-232).
#[derive(Debug, Default)]
pub struct MouseParser {
    mouse_buttons_pressed: std::collections::BTreeSet<i32>,
}

impl MouseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// `reset()` (parse.mouse.ts:32-34).
    pub fn reset(&mut self) {
        self.mouse_buttons_pressed.clear();
    }

    /// `parseMouseEvent(data)` (parse.mouse.ts:43-47).
    #[must_use]
    pub fn parse_mouse_event(&mut self, data: &[u8]) -> Option<RawMouseEvent> {
        self.parse_mouse_sequence_at(data, 0).map(|p| p.event)
    }

    /// The consumed byte length of the mouse sequence at `offset`
    /// (the feed-side view of `parseMouseSequenceAt`).
    pub fn parse_mouse_sequence_len(&mut self, data: &[u8], offset: usize) -> Option<usize> {
        self.parse_mouse_sequence_at(data, offset)
            .map(|p| p.consumed)
    }

    /// `parseAllMouseEvents(data)` (parse.mouse.ts:49-67): consecutive
    /// mouse sequences from one buffer; stops at the first non-mouse
    /// byte run.
    #[must_use]
    pub fn parse_all_mouse_events(&mut self, data: &[u8]) -> Vec<RawMouseEvent> {
        let mut events = Vec::new();
        let mut offset = 0usize;
        while offset < data.len() {
            match self.parse_mouse_sequence_at(data, offset) {
                Some(parsed) => {
                    events.push(parsed.event);
                    offset += parsed.consumed;
                }
                None => break,
            }
        }
        events
    }

    /// `parseMouseSequenceAt` (parse.mouse.ts:69-82) — byte-level so
    /// the X10 payload (bytes ≥ 0x80) stays intact, matching the
    /// latin1 decode upstream.
    fn parse_mouse_sequence_at(
        &mut self,
        data: &[u8],
        offset: usize,
    ) -> Option<ParsedMouseSequence> {
        if !data[offset..].starts_with(b"\x1b[") {
            return None;
        }
        let introducer = *data.get(offset + 2)?;
        match introducer {
            b'<' => self.parse_sgr_sequence(data, offset),
            b'M' => self.parse_basic_sequence(data, offset),
            _ => None,
        }
    }

    /// `parseSgrSequence` (parse.mouse.ts:84-124).
    fn parse_sgr_sequence(&mut self, data: &[u8], offset: usize) -> Option<ParsedMouseSequence> {
        let mut index = offset + 3;
        let mut values = [0u32; 3];
        let mut part = 0usize;
        let mut has_digit = false;

        while index < data.len() {
            let byte = data[index];
            if byte.is_ascii_digit() {
                has_digit = true;
                values[part] = values[part]
                    .wrapping_mul(10)
                    .wrapping_add(u32::from(byte - b'0'));
                index += 1;
                continue;
            }
            match byte {
                b';' => {
                    if !has_digit || part >= 2 {
                        return None;
                    }
                    part += 1;
                    has_digit = false;
                    index += 1;
                }
                b'M' | b'm' => {
                    if !has_digit || part != 2 {
                        return None;
                    }
                    let press_release = byte == b'M';
                    return Some(ParsedMouseSequence {
                        event: self.decode_sgr_event(
                            values[0],
                            values[1],
                            values[2],
                            press_release,
                        ),
                        consumed: index - offset + 1,
                    });
                }
                _ => return None,
            }
        }
        None
    }

    /// `parseBasicSequence` (parse.mouse.ts:126-139) — `ESC [ M` + 3
    /// payload bytes.
    fn parse_basic_sequence(&mut self, data: &[u8], offset: usize) -> Option<ParsedMouseSequence> {
        if offset + 6 > data.len() {
            return None;
        }
        // Payload bytes are raw (latin1 semantics): charCodeAt on the
        // latin1 string equals the byte value.
        let button_byte = i32::from(data[offset + 3]) - 32;
        // Convert from 1-based to 0-based
        let x = i32::from(data[offset + 4]) - 33;
        let y = i32::from(data[offset + 5]) - 33;
        Some(ParsedMouseSequence {
            event: self.decode_basic_event(button_byte, x, y),
            consumed: 6,
        })
    }

    /// `decodeSgrEvent` (parse.mouse.ts:141-190).
    fn decode_sgr_event(
        &mut self,
        raw_button_code: u32,
        wire_x: u32,
        wire_y: u32,
        press: bool,
    ) -> RawMouseEvent {
        let button = (raw_button_code & 3) as i32;
        let is_scroll = (raw_button_code & 64) != 0;
        let scroll_direction = if !is_scroll {
            None
        } else {
            scroll_direction(raw_button_code & 3)
        };

        let is_motion = (raw_button_code & 32) != 0;
        let modifiers = MouseModifiers {
            shift: (raw_button_code & 4) != 0,
            alt: (raw_button_code & 8) != 0,
            ctrl: (raw_button_code & 16) != 0,
        };

        let event_type;
        let mut scroll_info = None;

        if is_motion {
            let is_dragging = !self.mouse_buttons_pressed.is_empty();
            if button == 3 {
                event_type = MouseEventType::Move;
            } else if is_dragging {
                event_type = MouseEventType::Drag;
            } else {
                event_type = MouseEventType::Move;
            }
        } else if is_scroll && press {
            event_type = MouseEventType::Scroll;
            scroll_info = Some(ScrollInfo {
                direction: scroll_direction.unwrap_or(ScrollDirection::Up),
                delta: 1,
            });
        } else {
            event_type = if press {
                MouseEventType::Down
            } else {
                MouseEventType::Up
            };
            if event_type == MouseEventType::Down && button != 3 {
                self.mouse_buttons_pressed.insert(button);
            } else if event_type == MouseEventType::Up {
                self.mouse_buttons_pressed.remove(&button);
            }
        }

        RawMouseEvent {
            event_type,
            button: if button == 3 { 0 } else { button },
            x: wire_x as i32 - 1,
            y: wire_y as i32 - 1,
            modifiers,
            scroll: scroll_info,
        }
    }

    /// `decodeBasicEvent` (parse.mouse.ts:192-231).
    fn decode_basic_event(&mut self, button_byte: i32, x: i32, y: i32) -> RawMouseEvent {
        let button = button_byte & 3;
        let is_scroll = (button_byte & 64) != 0;
        let is_motion = (button_byte & 32) != 0;
        let scroll_direction = if !is_scroll {
            None
        } else {
            scroll_direction((button_byte & 3) as u32)
        };

        let modifiers = MouseModifiers {
            shift: (button_byte & 4) != 0,
            alt: (button_byte & 8) != 0,
            ctrl: (button_byte & 16) != 0,
        };

        let event_type;
        let actual_button;
        let mut scroll_info = None;

        if is_motion {
            event_type = MouseEventType::Move;
            actual_button = if button == 3 { -1 } else { button };
        } else if is_scroll {
            event_type = MouseEventType::Scroll;
            actual_button = 0;
            scroll_info = Some(ScrollInfo {
                direction: scroll_direction.unwrap_or(ScrollDirection::Up),
                delta: 1,
            });
        } else {
            event_type = if button == 3 {
                MouseEventType::Up
            } else {
                MouseEventType::Down
            };
            actual_button = if button == 3 { 0 } else { button };
        }

        RawMouseEvent {
            event_type,
            button: actual_button,
            x,
            y,
            modifiers,
            scroll: scroll_info,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sgr(bytes: &str) -> Option<RawMouseEvent> {
        MouseParser::new().parse_mouse_event(bytes.as_bytes())
    }

    #[test]
    fn sgr_press_release() {
        // parse.mouse.test.ts — left down/up at (9, 4) (1-based wire 10;5)
        let down = sgr("\x1b[<0;10;5M").unwrap();
        assert_eq!(down.event_type, MouseEventType::Down);
        assert_eq!(down.button, 0);
        assert_eq!(down.x, 9);
        assert_eq!(down.y, 4);
        let up = sgr("\x1b[<0;10;5m").unwrap();
        assert_eq!(up.event_type, MouseEventType::Up);
        assert_eq!(up.button, 0);
        // middle/right
        assert_eq!(sgr("\x1b[<1;1;1M").unwrap().button, 1);
        assert_eq!(sgr("\x1b[<2;1;1M").unwrap().button, 2);
    }

    #[test]
    fn sgr_scroll() {
        let up = sgr("\x1b[<64;10;5M").unwrap();
        assert_eq!(up.event_type, MouseEventType::Scroll);
        assert_eq!(up.scroll.unwrap().direction, ScrollDirection::Up);
        let down = sgr("\x1b[<65;10;5M").unwrap();
        assert_eq!(down.scroll.unwrap().direction, ScrollDirection::Down);
        // scroll release is NOT a scroll event (press === "M" required)
        let release = sgr("\x1b[<64;10;5m").unwrap();
        assert_eq!(release.event_type, MouseEventType::Up);
        assert!(release.scroll.is_none());
    }

    #[test]
    fn sgr_motion_and_drag() {
        let mut parser = MouseParser::new();
        // motion with no buttons pressed → move
        let move_evt = parser.parse_mouse_event(b"\x1b[<35;10;5M").unwrap();
        assert_eq!(move_evt.event_type, MouseEventType::Move);
        // button 3 in motion → move regardless
        // press, then motion with button held → drag
        parser.parse_mouse_event(b"\x1b[<0;5;5M").unwrap();
        let drag = parser.parse_mouse_event(b"\x1b[<32;10;6M").unwrap();
        assert_eq!(drag.event_type, MouseEventType::Drag);
        // release, then motion again → move
        parser.parse_mouse_event(b"\x1b[<0;5;5m").unwrap();
        let move2 = parser.parse_mouse_event(b"\x1b[<32;10;6M").unwrap();
        assert_eq!(move2.event_type, MouseEventType::Move);
    }

    #[test]
    fn sgr_modifiers() {
        let m = sgr("\x1b[<4;1;1M").unwrap(); // shift + left
        assert!(m.modifiers.shift && !m.modifiers.alt && !m.modifiers.ctrl);
        let m = sgr("\x1b[<8;1;1M").unwrap(); // alt
        assert!(m.modifiers.alt);
        let m = sgr("\x1b[<16;1;1M").unwrap(); // ctrl
        assert!(m.modifiers.ctrl);
    }

    #[test]
    fn x10_basic_sequences() {
        let mut parser = MouseParser::new();
        // ESC [ M Cb Cx Cy — button byte 32+0 (' '), x byte 33+9 ('*'),
        // y byte 33+4 ('%') → left down at (9, 4)
        let evt = parser.parse_mouse_event(b"\x1b[M *%").unwrap();
        assert_eq!(evt.event_type, MouseEventType::Down);
        assert_eq!(evt.button, 0);
        assert_eq!(evt.x, 9);
        assert_eq!(evt.y, 4);
        // release is button byte 32+3 ('#')
        let rel = parser.parse_mouse_event(b"\x1b[M#*%").unwrap();
        assert_eq!(rel.event_type, MouseEventType::Up);
        assert_eq!(rel.button, 0);
        // incomplete
        assert!(parser.parse_mouse_event(b"\x1b[M !").is_none());
        // payload bytes >= 0x80 stay intact (latin1 semantics)
        let far = parser.parse_mouse_event(b"\x1b[M \xff\xff").unwrap();
        assert_eq!(far.x, 0xff - 33);
        assert_eq!(far.y, 0xff - 33);
    }

    #[test]
    fn parse_all_consumes_runs() {
        let mut parser = MouseParser::new();
        let events = parser.parse_all_mouse_events(b"\x1b[<0;1;1M\x1b[<0;1;1m\x1b[<64;1;1M");
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, MouseEventType::Down);
        assert_eq!(events[1].event_type, MouseEventType::Up);
        assert_eq!(events[2].event_type, MouseEventType::Scroll);
        // stops at non-mouse data
        let events = parser.parse_all_mouse_events(b"\x1b[<0;1;1Mabc");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn rejects_non_mouse() {
        let mut parser = MouseParser::new();
        assert!(parser.parse_mouse_event(b"a").is_none());
        assert!(parser.parse_mouse_event(b"\x1b[A").is_none());
        assert!(parser.parse_mouse_event(b"\x1b[<0;1M").is_none());
        assert!(parser.parse_mouse_event(b"\x1b[<0;1;1X").is_none());
        // reset clears pressed-button tracking
        let mut p = MouseParser::new();
        p.parse_mouse_event(b"\x1b[<0;1;1M").unwrap();
        p.reset();
        let m = p.parse_mouse_event(b"\x1b[<32;5;5M").unwrap();
        assert_eq!(m.event_type, MouseEventType::Move);
    }
}
