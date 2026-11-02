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
//! * the kitty graphics query (`_Gi=31337…`, terminal.zig:384) is A3
//!   (image lanes) — its DA1 sibling stays, so sixel detection still
//!   works;
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
///                      ansi.zig:395)
/// 2. `\x1b]10;?\x07\x1b]11;?\x07` queryThemeColors (terminal.zig:360,
///                      oscThemeQueries ansi.zig:397)
/// 3. `\x1b[>0q`        xtversion (terminal.zig:367-369, ansi.zig:342)
/// 4. `\x1b[?25l`       hideCursor  (terminal.zig:368)
/// 5. `\x1b[s`          saveCursorState (terminal.zig:369)
/// 6. `\x1bP+q4d73\x1b\\` xtgettcapMs (capabilityQueriesBase,
///                      ansi.zig:354-361)
/// 7. DECRQM set `\x1b[?1016$p \x1b[?2027$p \x1b[?2031$p \x1b[?1004$p
///    \x1b[?2004$p \x1b[?2026$p` (ansi.zig:344-350)
/// 8. `\x1b[?u`         csiUQuery (ansi.zig:352)
/// 9. `\x1b[c`          primaryDeviceAttrs / DA1 (terminal.zig:379-381,
///                      ansi.zig:339)
/// 10. `\x1b[u`         restoreCursorState (terminal.zig:399)
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

/// What sits at the front of the probe buffer.
#[derive(Debug, PartialEq, Eq)]
enum Front {
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

/// DCS replies: xtversion (`DCS >|…ST`, terminal.zig:1313-1321) and
/// xtgettcap (`DCS 1+r…ST`, terminal.zig:1389-1412).
fn dcs_front(buf: &[u8]) -> Front {
    match buf.windows(2).position(|w| w == b"\x1b\\") {
        Some(end) => Front::Complete {
            unit_len: end + 2,
        },
        None => Front::Partial,
    }
}

/// OSC 10/11 theme replies (OSC 11 `]11;rgb:…`, terminated by ST/BEL —
/// is_osc_response's terminator rule, parse_keypress.rs:606).
fn osc_theme_front(buf: &[u8]) -> Front {
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
    } else if buf.starts_with(b"\x1bP") {
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
        "iterm", "kitty", "alacritty", "wezterm", "contour", "foot", "rio",
        "ghostty", "tmux", "screen",
    ];
    NAMES.iter().any(|n| value.to_ascii_lowercase().contains(n))
}

/// `isHyperlinkTerm` (terminal.zig:1417-1425).
fn is_hyperlink_term(value: &str) -> bool {
    const NAMES: [&str; 6] = [
        "ghostty", "kitty", "wezterm", "alacritty", "foot", "iterm",
    ];
    NAMES.iter().any(|n| value.to_ascii_lowercase().contains(n))
}

/// Apply one complete probe-response unit to the capability snapshot.
/// Returns whether anything changed (the emit-on-change contract —
/// upstream re-emits CAPABILITIES after every response,
/// renderer.ts:3415; we dedup identical snapshots).
fn classify(unit: &[u8], caps: &mut TerminalCapabilities) -> bool {
    // Lane A2 commit 1: recognition + plumbing only — the response
    // classifiers (terminal.zig:1247-1446) land in commit 2.
    let _ = (unit, caps);
    false
}

/// Extract the rgb payload of an OSC 10/11 theme reply
/// (`]11;rgb:rrrr/gggg/bbbb`), folded to 8-bit channels.
fn osc_rgb(unit: &[u8]) -> Option<[u8; 3]> {
    let s = String::from_utf8_lossy(unit);
    let payload = s
        .trim_end_matches("\x1b\\")
        .trim_end_matches('\u{7}')
        .splitn(2, ';')
        .nth(1)?;
    let payload = payload.strip_prefix("rgb:")?;
    let mut out = [0u8; 3];
    for (i, chan) in payload.split('/').enumerate() {
        if i > 2 {
            return None;
        }
        let chan = chan.get(..4)?; // 16-bit hex channels
        let v = u16::from_str_radix(chan, 16).ok()?;
        out[i] = (v >> 8) as u8;
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
    pub fn finish(
        &mut self,
        renderer: &mut CliRenderer,
        events: &mut Vec<CliRenderEvent>,
    ) {
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

    /// The burst is byte-exact vs queryTerminalSend's gated subset
    /// (each span's line citation in write_probe_burst's docs).
    #[test]
    fn probe_burst_exact_bytes() {
        let mut buf = Vec::new();
        write_probe_burst(&mut buf).unwrap();
        assert_eq!(
            buf,
            b"\x1b[?2031h" // colorSchemeUpdates(true)   terminal.zig:357
                .iter().cloned()
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
                .chain(b"\x1b[c".iter().cloned()) // DA1   terminal.zig:381
                .chain(b"\x1b[u".iter().cloned()) // restore terminal.zig:399
                .collect::<Vec<u8>>()[..]
        );
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
}
