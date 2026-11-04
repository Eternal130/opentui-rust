//! Capability probing: the startup query burst and the response state
//! machine that accumulates [`TerminalCapabilities`].
//!
//! Port of `queryTerminalSend` (terminal.zig:347-399) and
//! `processCapabilityResponse` (terminal.zig:1247-1446). The TS facade
//! feeds each *complete* probe response to the zig classifier and then
//! emits `CliRenderEvents.CAPABILITIES` with the fresh snapshot
//! (renderer.ts:3405-3415); the Rust backend owns that boundary, so
//! [`CapabilityProbe::feed`] does both (classify + emit through
//! `CliRenderer::emit_terminal_event`).
//!
//! Burst divergences vs terminal.zig:347-399, all GAPS-registered in
//! `terminal/mod.rs`:
//! * the kitty graphics query (`_Gi=31337…`, terminal.zig:384) IS sent
//!   (A3) — unconditionally, where upstream additionally gates on
//!   `!skip_graphics_query && !isInTmux()` (terminal.zig:379); the
//!   tmux DCS passthrough wrappers stay deferred;
//! * the explicit-width / scaled-text probe block (terminal.zig:389-398,
//!   home + `OSC 66` + CPR pairs) is deferred — it moves the cursor
//!   mid-burst and feeds `caps.explicit_width`/`scaled_text`, which no
//!   consumer reads yet;
//! * the notification queries (`OSC 99`/`1337`, ansi.zig:360) and the
//!   foot variant branch (terminal.zig:377) ride the notification
//!   family, also deferred;
//! * `CSI ?996n` is never sent upstream either (terminal.zig:341-345).

use std::io;
use std::time::{Duration, Instant};

use crate::renderer::{CliRenderEvent, CliRenderer, TerminalCapabilities};

/// The lane-A2 probe timeout (the oracle's 5s bound; upstream has no
/// single deadline — its TS facade resolves waiters per response class,
/// renderer.ts:2286-2314 — the timeout here is the Rust contract for
/// "unknown fields default").
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Probe burst — queryTerminalSend (terminal.zig:347-399), gated subset
// ---------------------------------------------------------------------------

/// Write the capability-query burst. Byte order is
/// `queryTerminalSend`'s:
///
/// 1. `\x1b[?2031h`     colorSchemeUpdates(true)  (terminal.zig:357,
///    ansi.zig:395)
/// 2. `\x1b]10;?\x07\x1b]11;?\x07` queryThemeColors (terminal.zig:360,
///    oscThemeQueries ansi.zig:397)
/// 3. `\x1b[>0q`        xtversion (terminal.zig:367-369, ansi.zig:342)
/// 4. `\x1b[?25l`       hideCursor  (terminal.zig:368)
/// 5. `\x1b[s`          saveCursorState (terminal.zig:369)
/// 6. `\x1bP+q4d73\x1b\\` xtgettcapMs (capabilityQueriesBase,
///    ansi.zig:354-361)
/// 7. DECRQM set `\x1b[?1016$p \x1b[?2027$p \x1b[?2031$p \x1b[?1004$p
///    \x1b[?2004$p \x1b[?2026$p` (ansi.zig:344-350)
/// 8. `\x1b[?u`         csiUQuery (ansi.zig:352)
/// 9. `\x1b_Gi=31337,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\`
///    kittyGraphicsQuery (terminal.zig:379-382, ansi.zig:351) —
///    ahead of DA1 exactly like upstream's `!skip_graphics_query &&
///    !isInTmux()` branch, but sent unconditionally (divergence
///    registered in the module docs)
/// 10. `\x1b[c`          primaryDeviceAttrs / DA1 (terminal.zig:379-381,
///     ansi.zig:339)
/// 11. `\x1b[u`          restoreCursorState (terminal.zig:399)
pub fn write_probe_burst(out: &mut dyn io::Write) -> io::Result<()> {
    out.write_all(b"\x1b[?2031h")?; // terminal.zig:357
    out.write_all(b"\x1b]10;?\x07\x1b]11;?\x07")?; // terminal.zig:360
    out.write_all(b"\x1b[>0q\x1b[?25l\x1b[s")?; // terminal.zig:367-369
    out.write_all(b"\x1bP+q4d73\x1b\\")?; // xtgettcapMs ansi.zig:354
    out.write_all(b"\x1b[?1016$p")?; // decrqmSgrPixels ansi.zig:344
    out.write_all(b"\x1b[?2027$p")?; // decrqmUnicode ansi.zig:345
    out.write_all(b"\x1b[?2031$p")?; // decrqmColorScheme ansi.zig:346
    out.write_all(b"\x1b[?1004$p")?; // decrqmFocus ansi.zig:347
    out.write_all(b"\x1b[?2004$p")?; // decrqmBracketedPaste ansi.zig:348
    out.write_all(b"\x1b[?2026$p")?; // decrqmSync ansi.zig:349
    out.write_all(b"\x1b[?u")?; // csiUQuery ansi.zig:352
                                // kitty graphics query (terminal.zig:379-382, ansi.zig:351) —
                                // upstream's constant, byte for byte, ahead of DA1.
    out.write_all(b"\x1b_Gi=31337,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\")?;
    out.write_all(b"\x1b[c")?; // DA1 terminal.zig:381
    out.write_all(b"\x1b[u") // restoreCursorState terminal.zig:399
}

// ---------------------------------------------------------------------------
// Response recognition — the streaming unit classes
// ---------------------------------------------------------------------------
//
// The unit predicates mirror the private completeness predicates in
// parse_keypress.rs (`is_device_attributes_response` :582,
// `is_mode_report` :593, `is_osc_response` :606) plus the DCS and
// kitty-query classes upstream handles inside processCapabilityResponse
// (terminal.zig:1247-1446). The PARTIAL variants are the streaming
// seam — a response split across reads stays pending exactly like
// parse_mouse's partial-prefix handling.

/// What sits at the front of the probe buffer (shared with the A4
/// theme watch's reuse of `osc_theme_front`).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Front {
    /// A complete probe response of `unit_len` bytes.
    Complete { unit_len: usize },
    /// A strict prefix of a possible probe response — hold for more
    /// bytes.
    Partial,
    /// Not a probe response — flush to the renderer's key/mouse feed.
    NotOurs,
}

fn is_param_byte(b: u8) -> bool {
    b.is_ascii_digit() || b == b';'
}

/// DA1 (`CSI ?..c`, parse_keypress.rs:582), DECRPM (`CSI ?..$y`,
/// parse_keypress.rs:593), kitty query reply (`CSI ?Nu`,
/// terminal.zig:1365-1377).
fn csi_question_front(buf: &[u8]) -> Front {
    // buf starts with "\x1b[?" (caller checked)
    let mut i = 3;
    while i < buf.len() && is_param_byte(buf[i]) {
        i += 1;
    }
    if i == 3 {
        // "\x1b[?" then a non-param byte: no response class starts
        // like that
        return Front::NotOurs;
    }
    match buf.get(i) {
        Some(b'c') => Front::Complete { unit_len: i + 1 }, // DA1
        Some(b'u') => Front::Complete { unit_len: i + 1 }, // kitty ?u
        Some(b'$') => match buf.get(i + 1) {
            Some(b'y') => Front::Complete { unit_len: i + 2 }, // DECRPM
            Some(_) => Front::NotOurs,
            None => Front::Partial,
        },
        Some(_) => Front::NotOurs,
        None => Front::Partial,
    }
}

/// Pixel-resolution reply (`ESC [ 4 ; height ; width t`,
/// renderer.ts:3579 / terminal-capability-detection.ts:81-99) — the
/// complete-unit predicate. Exactly two numeric groups; anything else
/// after `\x1b[4;` is not ours (e.g. a `\x1b[4~` End-key sequence).
fn pixel_resolution_front(buf: &[u8]) -> Front {
    let mut i = 4; // after "\x1b[4;"
    let mut groups = 1;
    let mut saw_digit = false;
    loop {
        match buf.get(i) {
            None => return Front::Partial,
            Some(byte) if byte.is_ascii_digit() => {
                saw_digit = true;
                i += 1;
            }
            Some(b';') => {
                if !saw_digit || groups >= 2 {
                    return Front::NotOurs;
                }
                groups += 1;
                saw_digit = false;
                i += 1;
            }
            Some(b't') => {
                if groups == 2 && saw_digit {
                    return Front::Complete { unit_len: i + 1 };
                }
                return Front::NotOurs;
            }
            Some(_) => return Front::NotOurs,
        }
    }
}

/// DCS replies: xtversion (`DCS >|…ST`, terminal.zig:1313-1321) and
/// xtgettcap (`DCS 1+r…ST`, terminal.zig:1389-1412).
fn dcs_front(buf: &[u8]) -> Front {
    match buf.windows(2).position(|w| w == b"\x1b\\") {
        Some(end) => Front::Complete { unit_len: end + 2 },
        None => Front::Partial,
    }
}

/// OSC 10/11 theme replies (OSC 11 `]11;rgb:…`, terminated by ST/BEL —
/// is_osc_response's terminator rule, parse_keypress.rs:606). Shared
/// with the post-probe theme watch (A4).
pub(super) fn osc_theme_front(buf: &[u8]) -> Front {
    for (i, b) in buf.iter().enumerate().skip(5) {
        match b {
            b'\x07' => return Front::Complete { unit_len: i + 1 },
            b'\x1b' if buf.get(i + 1) == Some(&b'\\') => {
                return Front::Complete { unit_len: i + 2 }
            }
            _ => {}
        }
    }
    Front::Partial
}

fn front_kind(buf: &[u8]) -> Front {
    if buf.starts_with(b"\x1b[?") {
        csi_question_front(buf)
    } else if buf.starts_with(b"\x1b[4;") {
        pixel_resolution_front(buf)
    } else if buf.starts_with(b"\x1bP") || buf.starts_with(b"\x1b_G") {
        // DCS replies and the kitty graphics APC reply share the ST
        // terminator scan (parseKittyGraphicsResponse,
        // terminal.zig:1106-1109).
        dcs_front(buf)
    } else if buf.starts_with(b"\x1b]10;") || buf.starts_with(b"\x1b]11;") {
        osc_theme_front(buf)
    } else {
        Front::NotOurs
    }
}

// ---------------------------------------------------------------------------
// Classification — processCapabilityResponse (terminal.zig:1247-1446)
// ---------------------------------------------------------------------------

/// `isOsc52Term` (terminal.zig:1404-1415).
fn is_osc52_term(value: &str) -> bool {
    const NAMES: [&str; 10] = [
        "iterm",
        "kitty",
        "alacritty",
        "wezterm",
        "contour",
        "foot",
        "rio",
        "ghostty",
        "tmux",
        "screen",
    ];
    NAMES.iter().any(|n| value.to_ascii_lowercase().contains(n))
}

/// `isHyperlinkTerm` (terminal.zig:1417-1425).
fn is_hyperlink_term(value: &str) -> bool {
    const NAMES: [&str; 6] = ["ghostty", "kitty", "wezterm", "alacritty", "foot", "iterm"];
    NAMES.iter().any(|n| value.to_ascii_lowercase().contains(n))
}

/// Apply one complete probe-response unit to the capability snapshot.
/// Returns whether anything changed (the emit-on-change contract —
/// upstream re-emits CAPABILITIES after every response,
/// renderer.ts:3415; we dedup identical snapshots).
fn classify(unit: &[u8], caps: &mut TerminalCapabilities) -> bool {
    let before = caps.clone();
    let unit_str = String::from_utf8_lossy(unit);

    // DECRPM substring branches (terminal.zig:1255-1271)
    if unit_str.contains("1016;2$y") {
        caps.sgr_pixels = true; // terminal.zig:1255-1256
    }
    if unit_str.contains("2027;2$y") {
        caps.unicode = crate::renderer::WidthMethod::Unicode; // :1257-1259
    }
    if unit_str.contains("2031;1$y") || unit_str.contains("2031;2$y") {
        caps.color_scheme_updates = true; // :1259-1261
    }
    if unit_str.contains("1004;1$y") || unit_str.contains("1004;2$y") {
        caps.focus_tracking = true; // :1262-1264
    }
    if unit_str.contains("2026;1$y") || unit_str.contains("2026;2$y") {
        caps.sync = true; // :1265-1267
    }
    if unit_str.contains("2004;1$y") || unit_str.contains("2004;2$y") {
        caps.bracketed_paste = true; // :1268-1270
    }

    // DA1: attribute `4` means sixel (parseSixelDeviceAttributes,
    // terminal.zig:1122-1140)
    if unit.starts_with(b"\x1b[?") && unit.ends_with(b"c") {
        for param in unit_str
            .trim_end_matches('c')
            .trim_start_matches("\x1b[?")
            .split(';')
        {
            if param == "4" {
                caps.sixel = true; // terminal.zig:1134-1136
            }
        }
    }

    // Pixel resolution: `CSI 4;height;width t` (renderer.ts:3579,
    // terminal-capability-detection.ts:89-99). A shape-valid reply with
    // out-of-range dimensions parses to None and leaves the captured
    // resolution untouched — the upstream "oversized → blocks" case.
    if unit.starts_with(b"\x1b[4;") && unit.ends_with(b"t") {
        if let Some(resolution) = crate::image_render::parse_pixel_resolution(&unit_str) {
            caps.pixel_resolution = Some((resolution.width, resolution.height));
        }
    }

    // xtversion: `DCS >|name version ST` (terminal.zig:1313-1321);
    // `parseXtversion` splits the name at '(' or ' ' — "kitty(0.40.1)",
    // "ghostty 1.1.3", "tmux 3.5a" (terminal.zig:1739-1770).
    if let Some(pos) = unit_str.find("\x1bP>|") {
        let start = pos + 4;
        if let Some(end) = unit_str[start..].find("\x1b\\") {
            let term_str = unit_str[start..start + end].trim();
            let (name, version) = match term_str.find(['(', ' ']) {
                Some(split) => {
                    let name = &term_str[..split];
                    let version = term_str[split..]
                        .trim_start_matches(['(', ' '])
                        .trim_end_matches(')');
                    (name, version)
                }
                None => (term_str, ""),
            };
            if !name.is_empty() {
                caps.terminal_name = Some(name.to_string());
                if !version.is_empty() {
                    caps.terminal_version = Some(version.to_string());
                }
            }
        }
    }

    // Exact kitty identity from a complete xtversion reply
    // (terminal.zig:1353-1360)
    if caps
        .terminal_name
        .as_deref()
        .is_some_and(|n| n.eq_ignore_ascii_case("kitty"))
    {
        caps.kitty_keyboard = true;
        caps.unicode = crate::renderer::WidthMethod::Unicode;
        caps.rgb = true;
        caps.ansi256 = true;
        caps.bracketed_paste = true;
        caps.hyperlinks = true;
    }

    // Kitty protocol via `CSI ?Nu` query reply (terminal.zig:1363-1377)
    if unit.starts_with(b"\x1b[?")
        && unit.ends_with(b"u")
        && unit[3..unit.len() - 1].iter().all(|b| b.is_ascii_digit())
        && unit.len() > 4
    {
        caps.kitty_keyboard = true; // terminal.zig:1374-1376
    }

    // Terminal-name heuristics (terminal.zig:1380-1387)
    if unit_str.contains("tmux") {
        caps.unicode = crate::renderer::WidthMethod::Wcwidth; // :1380-1382
        caps.explicit_cursor_positioning = true; // :1382
    }
    if unit_str.contains("alacritty") {
        caps.explicit_cursor_positioning = true; // :1385-1387
    }

    // xtgettcap OSC52 reply: `DCS 1+r 4d73 = <hex> ST`
    // (parseXtgettcapMs, terminal.zig:1389-1412)
    if unit_str.starts_with("\x1bP1+r") {
        let body = unit_str
            .trim_start_matches("\x1bP1+r")
            .trim_end_matches("\x1b\\");
        if let Some((key, value)) = body.split_once('=') {
            if key.eq_ignore_ascii_case("4d73")
                && !value.is_empty()
                && value.len() % 2 == 0
                && value.bytes().all(|b| b.is_ascii_hexdigit())
            {
                caps.osc52 = true; // terminal.zig:1405-1411
            }
        }
    }

    // Kitty graphics APC reply (`_Gi=…;…ST`) — the answer to the
    // `_Gi=31337` query in the burst (parseKittyGraphicsResponse,
    // terminal.zig:1103-1121): scan every `_G…ST` frame, split the
    // control options at ';' and by ',', and `i=31337` marks the
    // capability. Upstream gates on `graphics_enabled` (an
    // OPENTUI_GRAPHICS env override, terminal.zig:748+); the port has
    // no env-override layer, so the reply always classifies
    // (divergence registered in terminal/mod.rs).
    if let Some(start) = unit_str.find("\x1b_G") {
        if let Some(end) = unit_str[start + 3..].find("\x1b\\") {
            let frame = &unit_str[start + 3..start + 3 + end];
            let control = frame.split(';').next().unwrap_or("");
            if control.split(',').any(|f| f == "i=31337") {
                caps.kitty_graphics = true; // terminal.zig:1112-1116
            }
        }
    }

    // Name-based OSC52/hyperlink enablement (terminal.zig:1398-1425) —
    // scanned against the full unit (upstream scans the whole
    // capability-response buffer).
    if !caps.osc52 && is_osc52_term(&unit_str) {
        caps.osc52 = true;
    }
    if !caps.hyperlinks && is_hyperlink_term(&unit_str) {
        caps.hyperlinks = true;
    }

    *caps != before
}

/// Extract the rgb payload of an OSC 10/11 theme reply
/// (`]11;rgb:rrrr/gggg/bbbb`), folded to 8-bit channels with
/// upstream's `scaleOscThemeComponent` rule (round(value/max*255),
/// renderer-theme-mode.ts:8-13) — channel hex length 1..4, exactly
/// the widths upstream's regex accepts.
pub(super) fn osc_rgb(unit: &[u8]) -> Option<[u8; 3]> {
    let s = String::from_utf8_lossy(unit);
    let payload = s
        .trim_end_matches("\x1b\\")
        .trim_end_matches('\u{7}')
        .split_once(';')?
        .1;
    let payload = payload.strip_prefix("rgb:")?;
    let mut out = [0u8; 3];
    for (i, chan) in payload.split('/').enumerate() {
        if i > 2 || chan.is_empty() || chan.len() > 4 {
            return None;
        }
        if !chan.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let v = u16::from_str_radix(chan, 16).ok()?;
        let max = (1u32 << (4 * chan.len())) - 1;
        out[i] = ((u32::from(v) * 255 + max / 2) / max) as u8;
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// The state machine
// ---------------------------------------------------------------------------

/// The probe state machine: pending bytes, the accumulating snapshot,
/// and the deadline. Feed it raw stdin bytes; it routes probe
/// responses into [`TerminalCapabilities`] and hands everything else
/// back for the renderer's key/mouse feed.
pub struct CapabilityProbe {
    pending: Vec<u8>,
    caps: TerminalCapabilities,
    deadline: Instant,
    /// The terminal answered DA1 and at least one of the kitty/xt
    /// version probes — "capability-complete" (upstream has no such
    /// predicate; its TS facade resolves waiters per response class,
    /// renderer.ts:2286-2314 — this is the Rust bounded-wait contract).
    saw_da1: bool,
    saw_kitty_or_xtversion: bool,
    finished: bool,
    emitted: bool,
    /// Last OSC 11 background (theme detection input — the THEME_MODE /
    /// PALETTE events are gap-registered, upstream renderer.ts:761-762).
    pub last_osc11_bg: Option<[u8; 3]>,
    /// Last OSC 10 foreground, same story.
    pub last_osc10_fg: Option<[u8; 3]>,
}

impl CapabilityProbe {
    pub fn new(timeout: Duration) -> Self {
        Self {
            pending: Vec::new(),
            caps: TerminalCapabilities::default(),
            deadline: Instant::now() + timeout,
            saw_da1: false,
            saw_kitty_or_xtversion: false,
            finished: false,
            emitted: false,
            last_osc11_bg: None,
            last_osc10_fg: None,
        }
    }

    /// Whether the deadline passed (checked between feeds; the caller's
    /// pump loop drives time forward).
    #[must_use]
    pub fn timed_out(&self) -> bool {
        !self.finished && Instant::now() >= self.deadline
    }

    /// Whether the probe already produced its final snapshot.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// The accumulated snapshot so far.
    #[must_use]
    pub fn capabilities(&self) -> &TerminalCapabilities {
        &self.caps
    }

    fn emit(&mut self, renderer: &mut CliRenderer, events: &mut Vec<CliRenderEvent>) {
        let event = CliRenderEvent::Capabilities(self.caps.clone());
        renderer.emit_terminal_event(event.clone());
        events.push(event);
        self.emitted = true;
    }

    /// Finish the probe: flush any held partial bytes to the renderer's
    /// feed (they were never ours), emit the final snapshot if nothing
    /// was emitted yet (timeout = unknown fields default), and mark
    /// done. Idempotent.
    pub fn finish(&mut self, renderer: &mut CliRenderer, events: &mut Vec<CliRenderEvent>) {
        if self.finished {
            return;
        }
        self.finished = true;
        if !self.pending.is_empty() {
            let passthrough = std::mem::take(&mut self.pending);
            renderer.feed_bytes(&passthrough);
        }
        if !self.emitted {
            self.emit(renderer, events);
        }
    }

    /// Feed raw stdin bytes: probe responses route into the capability
    /// model, everything else passes through to
    /// `renderer.feed_bytes` untouched. Emits `Capabilities` events on
    /// every snapshot change, and finishes on capability-completeness
    /// or timeout.
    pub fn feed(
        &mut self,
        renderer: &mut CliRenderer,
        bytes: &[u8],
        events: &mut Vec<CliRenderEvent>,
    ) {
        if self.finished {
            renderer.feed_bytes(bytes);
            return;
        }
        self.pending.extend_from_slice(bytes);

        loop {
            match front_kind(&self.pending) {
                Front::Complete { unit_len } => {
                    let unit: Vec<u8> = self.pending.drain(..unit_len).collect();
                    if unit.starts_with(b"\x1b[?") && unit.ends_with(b"c") {
                        self.saw_da1 = true;
                    }
                    if unit.starts_with(b"\x1b[?") && unit.ends_with(b"u") {
                        self.saw_kitty_or_xtversion = true;
                    }
                    if unit.starts_with(b"\x1bP>|") {
                        self.saw_kitty_or_xtversion = true;
                    }
                    if unit.starts_with(b"\x1b]11;") {
                        self.last_osc11_bg = osc_rgb(&unit);
                    }
                    if unit.starts_with(b"\x1b]10;") {
                        self.last_osc10_fg = osc_rgb(&unit);
                    }
                    if classify(&unit, &mut self.caps) {
                        self.emit(renderer, events);
                    }
                }
                Front::Partial => break, // wait for the rest of the read
                Front::NotOurs => {
                    let passthrough = std::mem::take(&mut self.pending);
                    renderer.feed_bytes(&passthrough);
                    break;
                }
            }
            if self.pending.is_empty() {
                break;
            }
        }

        // Capability-complete: DA1 answered and a kitty/xtversion probe
        // answered (or identified as absent via another reply), with
        // nothing left pending.
        let ready = self.saw_da1 && self.saw_kitty_or_xtversion && self.pending.is_empty();
        if ready || self.timed_out() {
            self.finish(renderer, events);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal renderer helper (config defaults are fine — the probe
    /// never reads layout).
    fn renderer() -> CliRenderer {
        crate::renderer::create_cli_renderer(crate::renderer::CliRendererConfig::default())
    }

    /// The pixel-resolution query is a separate startup write
    /// (renderer.ts:3343/3959-3965), not part of queryTerminalSend.
    #[test]
    fn pixel_resolution_query_and_reply() {
        let mut query = Vec::new();
        crate::image_render::write_pixel_resolution_query(&mut query).unwrap();
        assert_eq!(query, b"\x1b[14t");

        let mut caps = crate::renderer::TerminalCapabilities::default();
        assert!(classify(b"\x1b[4;720;1280t", &mut caps));
        assert_eq!(caps.pixel_resolution, Some((1280, 720)));
        // Shape-valid but out of i32 range: the reply is consumed, the
        // captured resolution keeps the last good value (upstream's
        // "oversized resolution leaves images on blocks" when it is the
        // first reply = None).
        assert!(!classify(b"\x1b[4;4294967295;4294967295t", &mut caps));
        assert_eq!(caps.pixel_resolution, Some((1280, 720)));
        let mut fresh = crate::renderer::TerminalCapabilities::default();
        assert!(!classify(b"\x1b[4;4294967295;4294967295t", &mut fresh));
        assert_eq!(fresh.pixel_resolution, None);

        assert_eq!(pixel_resolution_front(b"\x1b[4;80;80"), Front::Partial);
        assert_eq!(
            pixel_resolution_front(b"\x1b[4;80;80t"),
            Front::Complete { unit_len: 10 }
        );
        assert_eq!(pixel_resolution_front(b"\x1b[4;~"), Front::NotOurs);
        assert_eq!(pixel_resolution_front(b"\x1b[4;;t"), Front::NotOurs);
    }

    /// The burst is byte-exact vs queryTerminalSend's gated subset
    /// (each span's line citation in write_probe_burst's docs).
    #[test]
    fn probe_burst_exact_bytes() {
        let mut buf = Vec::new();
        write_probe_burst(&mut buf).unwrap();
        assert_eq!(
            buf,
            b"\x1b[?2031h" // colorSchemeUpdates(true)   terminal.zig:357
                .iter()
                .cloned()
                .chain(b"\x1b]10;?\x07\x1b]11;?\x07".iter().cloned()) // :360
                .chain(b"\x1b[>0q\x1b[?25l\x1b[s".iter().cloned()) // :367-369
                .chain(b"\x1bP+q4d73\x1b\\".iter().cloned()) // ansi.zig:354
                .chain(b"\x1b[?1016$p".iter().cloned()) // ansi.zig:344
                .chain(b"\x1b[?2027$p".iter().cloned()) // ansi.zig:345
                .chain(b"\x1b[?2031$p".iter().cloned()) // ansi.zig:346
                .chain(b"\x1b[?1004$p".iter().cloned()) // ansi.zig:347
                .chain(b"\x1b[?2004$p".iter().cloned()) // ansi.zig:348
                .chain(b"\x1b[?2026$p".iter().cloned()) // ansi.zig:349
                .chain(b"\x1b[?u".iter().cloned()) // ansi.zig:352
                .chain(
                    b"\x1b_Gi=31337,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\"
                        .iter()
                        .cloned()
                ) // kitty gfx terminal.zig:384, ansi.zig:351
                .chain(b"\x1b[c".iter().cloned()) // DA1   terminal.zig:381
                .chain(b"\x1b[u".iter().cloned()) // restore terminal.zig:399
                .collect::<Vec<u8>>()[..]
        );
    }

    /// A canned ghostty-shaped reply set classifies field-by-field
    /// (every branch cites its zig line inside `classify`).
    #[test]
    fn canned_responses_classify_per_field() {
        let mut probe = CapabilityProbe::new(Duration::from_secs(5));
        let mut r = renderer();
        let mut events = Vec::new();
        // DECRPM acks: sync + focus + paste + colorScheme + sgr pixels,
        // plus the OSC 11 dark background reply.
        probe.feed(
            &mut r,
            b"\x1b[?2026;2$y\x1b[?1004;1$y\x1b[?2004;1$y\x1b[?2031;1$y\x1b[?1016;2$y\x1b]11;rgb:1010/1010/1010\x07",
            &mut events,
        );
        // kitty query reply + xtversion + DA1 with sixel attr 4
        probe.feed(
            &mut r,
            b"\x1b[?1u\x1bP>|ghostty 1.1.3\x1b\\\x1b[?62;4;6;9;15;22c",
            &mut events,
        );

        assert!(probe.is_finished(), "DA1 + kitty reply completes the probe");
        let caps = probe.capabilities();
        assert!(caps.sync, "2026;2$y -> sync (terminal.zig:1265-1267)");
        assert!(caps.focus_tracking, "1004;1$y (terminal.zig:1262-1264)");
        assert!(caps.bracketed_paste, "2004;1$y (terminal.zig:1268-1270)");
        assert!(
            caps.color_scheme_updates,
            "2031;1$y (terminal.zig:1259-1261)"
        );
        assert!(caps.sgr_pixels, "1016;2$y (terminal.zig:1255-1256)");
        assert!(caps.kitty_keyboard, "CSI ?1u (terminal.zig:1365-1377)");
        assert_eq!(
            caps.terminal_name.as_deref(),
            Some("ghostty"),
            "xtversion name token (terminal.zig:1755-1766)"
        );
        assert_eq!(
            caps.terminal_version.as_deref(),
            Some("1.1.3"),
            "xtversion version token (terminal.zig:1757-1766)"
        );
        assert!(caps.sixel, "DA1 attr 4 (terminal.zig:1122-1140)");
        assert!(caps.osc52, "ghostty isOsc52Term (terminal.zig:1404-1415)");
        assert!(
            caps.hyperlinks,
            "ghostty isHyperlinkTerm (terminal.zig:1417-1425)"
        );
        assert_eq!(probe.last_osc11_bg, Some([0x10, 0x10, 0x10]));
        // at least one Capabilities event fired
        assert!(events
            .iter()
            .any(|e| matches!(e, CliRenderEvent::Capabilities(_))));
    }

    /// kitty xtversion identity flips the exact-feature set
    /// (terminal.zig:1353-1360).
    #[test]
    fn kitty_xtversion_identity() {
        let mut probe = CapabilityProbe::new(Duration::from_secs(5));
        let mut r = renderer();
        let mut events = Vec::new();
        probe.feed(
            &mut r,
            b"\x1bP>|kitty(0.40.1)\x1b\\\x1b[?1u\x1b[?62;1;2;6;9;15;22c",
            &mut events,
        );
        let caps = probe.capabilities();
        assert!(caps.kitty_keyboard && caps.rgb && caps.ansi256);
        assert!(caps.bracketed_paste && caps.hyperlinks);
        assert_eq!(caps.unicode, crate::renderer::WidthMethod::Unicode);
    }

    /// tmux xtversion pins wcwidth + explicit cursor positioning
    /// (terminal.zig:1380-1383).
    #[test]
    fn tmux_xtversion() {
        let mut probe = CapabilityProbe::new(Duration::from_secs(5));
        let mut r = renderer();
        let mut events = Vec::new();
        probe.feed(
            &mut r,
            b"\x1bP>|tmux 3.5a\x1b\\\x1b[?0u\x1b[?1;2c",
            &mut events,
        );
        let caps = probe.capabilities();
        assert_eq!(caps.unicode, crate::renderer::WidthMethod::Wcwidth);
        assert!(caps.explicit_cursor_positioning);
    }

    /// Timeout with zero responses: the snapshot is the all-unknown
    /// default and exactly one Capabilities event fires on finish.
    #[test]
    fn timeout_yields_unknown_defaults() {
        let mut probe = CapabilityProbe::new(Duration::from_millis(0));
        let mut r = renderer();
        let mut events = Vec::new();
        std::thread::sleep(Duration::from_millis(2));
        probe.feed(&mut r, b"", &mut events);
        assert!(probe.is_finished());
        assert_eq!(probe.capabilities(), &TerminalCapabilities::default());
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            CliRenderEvent::Capabilities(TerminalCapabilities::default())
        );
    }

    /// A response split across feeds (half a DA1) stays pending until
    /// its terminator arrives — the streaming seam.
    #[test]
    fn split_response_across_feeds() {
        let mut probe = CapabilityProbe::new(Duration::from_secs(5));
        let mut r = renderer();
        let mut events = Vec::new();
        probe.feed(&mut r, b"\x1b[?62;4", &mut events);
        assert!(!probe.is_finished());
        assert!(!probe.capabilities().sixel);
        probe.feed(&mut r, b";22c", &mut events);
        assert!(probe.capabilities().sixel, "completed DA1 attr 4");
    }

    /// xtgettcap OSC52 reply (terminal.zig:1389-1412).
    #[test]
    fn xtgettcap_osc52_reply() {
        let mut probe = CapabilityProbe::new(Duration::from_secs(5));
        let mut r = renderer();
        let mut events = Vec::new();
        probe.feed(
            &mut r,
            b"\x1bP1+r4d73=41\x1b\\\x1b[?0u\x1b[?1;2c",
            &mut events,
        );
        assert!(probe.capabilities().osc52);
    }

    /// The kitty graphics APC reply classifies caps.kitty_graphics
    /// (parseKittyGraphicsResponse, terminal.zig:1103-1121): the
    /// reply carries the queried id `i=31337` in its control field,
    /// possibly with siblings (kitty's `i=1` EINVAL shape below is
    /// terminal.zig:1096's canned kitty stream minus the prefix).
    #[test]
    fn kitty_graphics_apc_reply() {
        let mut probe = CapabilityProbe::new(Duration::from_secs(5));
        let mut r = renderer();
        let mut events = Vec::new();
        probe.feed(
            &mut r,
            b"\x1b_Gi=31337;OK\x1b\\\x1b[?0u\x1b[?62;22c",
            &mut events,
        );
        assert!(probe.capabilities().kitty_graphics);
    }

    /// A non-matching APC frame (kitty's error reply to a different
    /// query id) must NOT flip the capability — only `i=31337` does
    /// (terminal.zig:1112-1116, exact field equality).
    #[test]
    fn kitty_graphics_apc_wrong_id_ignored() {
        let mut probe = CapabilityProbe::new(Duration::from_secs(5));
        let mut r = renderer();
        let mut events = Vec::new();
        probe.feed(
            &mut r,
            b"\x1b_Gi=1;EINVAL:Zero width/height not allowed\x1b\\\x1b[?0u\x1b[?1;2c",
            &mut events,
        );
        assert!(!probe.capabilities().kitty_graphics);
    }
}
