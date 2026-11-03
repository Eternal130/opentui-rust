//! Renderer configuration + mode resolution family (moved verbatim
//! from the former single-file `renderer.rs`): `CliRendererConfig`,
//! `ScreenMode`/`ExternalOutputMode` and the `resolveModes` chain.
use super::*;


/// `stdin?: NodeJS.ReadStream` (renderer.ts:126-128) reduced to the
/// port's choice. Upstream duck-types any `Readable` (default
/// `process.stdin`); the live runtime reads through the Phase A
/// [`crate::terminal::Terminal`] session's `TtyDevice` instead
/// (`UnixTty` opens `/dev/tty`, falling back to a dup of stdin —
/// tty.rs `UnixTty::new`), so only the intent is configurable here.
/// `Disabled` = no live input feed (the embedder drives
/// [`CliRenderer::feed_bytes`] itself, like the headless harness).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StdinSource {
    /// `process.stdin` — the session's tty device (the default).
    #[default]
    Process,
    /// No live input stream.
    Disabled,
}

/// `ConsoleMode` (renderer.ts:265; the config field at
/// renderer.ts:222-225, default `"console-overlay"` set at
/// renderer.ts:1285).
///
/// This lane implements the **"disabled"** semantics only: the
/// built-in console overlay stays hidden (renderer.ts:1625-1632's
/// `console.deactivate()` arm). `"console-overlay"` is config-parity
/// — the overlay renderable family (console.ts) is still deferred, so
/// activating it records intent without painting anything.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConsoleMode {
    /// Capture `console.*` and show the overlay (deferred — the
    /// overlay renderable is unported).
    #[default]
    ConsoleOverlay,
    /// Hide the overlay (implemented: nothing captures or paints).
    Disabled,
}

/// `ConsoleAction` (console.ts:231-242) — the overlay's action ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleAction {
    ScrollUp,
    ScrollDown,
    ScrollToTop,
    ScrollToBottom,
    PositionPrevious,
    PositionNext,
    SizeIncrease,
    SizeDecrease,
    SaveLogs,
    CopySelection,
}

/// `ConsoleKeyBinding = BaseKeyBinding<ConsoleAction>`
/// (console.ts:243) — a keypress plus its overlay action, the
/// flattened `BaseKeyBinding` field set
/// (`{ name, ctrl?, shift?, meta?, super?, action }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleKeyBinding {
    pub name: String,
    pub ctrl: bool,
    pub shift: bool,
    pub meta: bool,
    pub super_key: bool,
    pub action: ConsoleAction,
}

impl ConsoleKeyBinding {
    /// `{ name, action }` — the unmodified-key shape most of
    /// `defaultConsoleKeybindings` uses (console.ts:245-256).
    #[must_use]
    pub fn new(name: &str, action: ConsoleAction) -> Self {
        Self {
            name: name.to_owned(),
            ctrl: false,
            shift: false,
            meta: false,
            super_key: false,
            action,
        }
    }
}

/// `ConsoleOptions` (console.ts:259-286) — the `consoleOptions` config
/// field (renderer.ts:203-204) reduced to the `keyBindings` slice;
/// every styling/layout knob rides the overlay family (deferred).
/// An empty `key_bindings` = upstream's `undefined`, i.e. the
/// `defaultConsoleKeybindings` table applies (console.ts:245-256).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConsoleOptions {
    pub key_bindings: Vec<ConsoleKeyBinding>,
}

/// `CliRendererConfig` (renderer.ts:122-243) — the slice subset.
#[derive(Debug, Clone)]
pub struct CliRendererConfig {
    pub width: u32,
    pub height: u32,
    /// renderer.ts:217 `backgroundColor`
    pub background_color: Rgba,
    /// `targetFps` (renderer.ts:169-172; `|| 30` at 1196)
    pub target_fps: f64,
    /// `maxFps` (renderer.ts:174-177; `|| 60` at 1197)
    pub max_fps: f64,
    /// `autoFocus` (renderer.ts:198, 1223) — focus the nearest
    /// focusable renderable on left click.
    pub auto_focus: bool,
    /// `useMouse` (renderer.ts:205-206, 1222).
    pub use_mouse: bool,
    /// `useKittyKeyboard` (renderer.ts:214) — `None` disables the
    /// kitty protocol; `Some(options)` enables it (the parser's
    /// `useKittyForParsing = kittyConfig !== null`, renderer.ts:1247).
    pub kitty_keyboard: Option<KittyKeyboardOptions>,
    /// `stdin` (renderer.ts:126-128) — see [`StdinSource`].
    pub stdin: StdinSource,
    /// `exitOnCtrlC` (renderer.ts:152-153) — destroy the renderer on
    /// Ctrl+C (armed at renderer.ts:1252-1257, default true).
    pub exit_on_ctrl_c: bool,
    /// `clearOnShutdown` (renderer.ts:158-159) — clear owned screen
    /// regions on destroy (default true).
    pub clear_on_shutdown: bool,
    /// `gatherStats` (renderer.ts:183) — collect frame timing stats
    /// for the debug overlay (default false; `setGatherStats` at
    /// renderer.ts:4924).
    pub gather_stats: bool,
    /// `consoleMode` (renderer.ts:222-225, default at :1285) — see
    /// [`ConsoleMode`].
    pub console_mode: ConsoleMode,
    /// `consoleOptions` (renderer.ts:203-204 → ConsoleOptions,
    /// console.ts:259-286) — the `keyBindings` slice.
    pub console_options: ConsoleOptions,
    /// The `EventEmitter.setMaxListeners` counterpart's threshold
    /// (node:events): the listener-warning cap, default 10 (Node's
    /// `defaultMaxListeners`).
    pub max_listeners: u32,
    /// `screenMode` (renderer.ts:202, default at :370) — where the
    /// renderer owns terminal space.
    pub screen_mode: ScreenMode,
    /// `footerHeight` (renderer.ts:205, default `DEFAULT_FOOTER_HEIGHT`
    /// at :341/:376) — the requested footer height for
    /// [`ScreenMode::SplitFooter`]; `None` upstream
    /// (`footerHeight === undefined`).
    pub footer_height: Option<u32>,
    /// `externalOutputMode` (renderer.ts:208, default at :379) — what
    /// happens to writes through the configured output; `None` is
    /// upstream's undefined (defaulted per screenMode in
    /// [`resolve_modes`]).
    pub external_output_mode: Option<ExternalOutputMode>,
}

impl Default for CliRendererConfig {
    fn default() -> Self {
        CliRendererConfig {
            width: 80,
            height: 24,
            background_color: Rgba::from_ints(0, 0, 0, 0),
            target_fps: 30.0,
            max_fps: 60.0,
            auto_focus: true,
            use_mouse: true,
            kitty_keyboard: None,
            stdin: StdinSource::Process,
            exit_on_ctrl_c: true,
            clear_on_shutdown: true,
            gather_stats: false,
            console_mode: ConsoleMode::default(),
            console_options: ConsoleOptions::default(),
            max_listeners: 10,
            screen_mode: ScreenMode::default(),
            footer_height: None,
            external_output_mode: None,
        }
    }
}

/// `ScreenMode` (renderer.ts:242) — controls how the renderer uses
/// terminal space:
///
/// * `AlternateScreen` — the terminal's alternate screen buffer
///   (default, renderer.ts:370);
/// * `MainScreen` — render on the main screen;
/// * `SplitFooter` — keep the renderer in a reserved footer on the
///   main screen (the split-footer scrollback family).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ScreenMode {
    #[default]
    AlternateScreen,
    MainScreen,
    SplitFooter,
}

/// `ExternalOutputMode` (renderer.ts:250) — controls writes through
/// the configured output:
///
/// * `CaptureStdout` — queue output and replay it above the split
///   footer; only valid with [`ScreenMode::SplitFooter`]
///   (renderer.ts:384-386);
/// * `Passthrough` — leave the output alone (default,
///   renderer.ts:379).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExternalOutputMode {
    #[default]
    Passthrough,
    CaptureStdout,
}

/// `DEFAULT_FOOTER_HEIGHT` (renderer.ts:341).
pub const DEFAULT_FOOTER_HEIGHT: u32 = 12;

/// `MAX_SCROLLBACK_SURFACE_HEIGHT_PASSES` (renderer.ts:342).
pub const MAX_SCROLLBACK_SURFACE_HEIGHT_PASSES: usize = 4;

/// The `resolveModes` failure modes — upstream throws
/// (renderer.ts:355-360, :384-386); the port returns the error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererModeError {
    /// `footerHeight must be greater than 0` (renderer.ts:359-360).
    FooterHeightNotPositive,
    /// `externalOutputMode "capture-stdout" requires screenMode
    /// "split-footer"` (renderer.ts:384-386).
    CaptureRequiresSplitFooter,
}

impl std::fmt::Display for RendererModeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FooterHeightNotPositive => {
                write!(f, "footerHeight must be greater than 0")
            }
            Self::CaptureRequiresSplitFooter => write!(
                f,
                "externalOutputMode \"capture-stdout\" requires screenMode \"split-footer\""
            ),
        }
    }
}

impl std::error::Error for RendererModeError {}

/// `normalizeFooterHeight` (renderer.ts:348-362) — `None` →
/// [`DEFAULT_FOOTER_HEIGHT`]; a value must be ≥ 1 (upstream's
/// non-finite check is impossible for `u32`).
#[must_use]
pub fn normalize_footer_height(footer_height: Option<u32>) -> Result<u32, RendererModeError> {
    match footer_height {
        None => Ok(DEFAULT_FOOTER_HEIGHT), // renderer.ts:349-351
        Some(h) if h >= 1 => Ok(h),        // Math.trunc is identity on u32
        Some(_) => Err(RendererModeError::FooterHeightNotPositive), // :359-360
    }
}

/// The `resolveModes` outcome (renderer.ts:364-393) — the triple every
/// mode-gated API (scrollback writes, surfaces, replay resets) checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedModes {
    pub screen_mode: ScreenMode,
    pub footer_height: u32,
    pub external_output_mode: ExternalOutputMode,
}

/// The boolean-env read behind `env.OTUI_USE_ALTERNATE_SCREEN` /
/// `env.OTUI_OVERRIDE_STDOUT` (env.ts's parse: present + not one of
/// the falsy spellings → true). Presence is what drives the override.
fn env_flag(name: &str) -> Option<bool> {
    let raw = std::env::var_os(name)?;
    let value = raw.to_string_lossy().to_ascii_lowercase();
    Some(!matches!(value.as_str(), "" | "0" | "false" | "no" | "off"))
}

/// `resolveModes(config)` (renderer.ts:365-386): the config enums with
/// their defaults and the `OTUI_USE_ALTERNATE_SCREEN` /
/// `OTUI_OVERRIDE_STDOUT` env overrides, validated (`capture-stdout`
/// requires `split-footer`). Deterministic for a given config + env;
/// tests that exercise the env arms must serialize behind a mutex (the
/// process env is global).
pub fn resolve_modes(config: &CliRendererConfig) -> Result<ResolvedModes, RendererModeError> {
    // renderer.ts:370-373
    let mut screen_mode = config.screen_mode;
    if let Some(use_alt) = env_flag("OTUI_USE_ALTERNATE_SCREEN") {
        screen_mode = if use_alt {
            ScreenMode::AlternateScreen
        } else {
            ScreenMode::MainScreen
        };
    }

    // renderer.ts:375-376
    let footer_height = if screen_mode == ScreenMode::SplitFooter {
        normalize_footer_height(config.footer_height)?
    } else {
        DEFAULT_FOOTER_HEIGHT
    };

    // renderer.ts:378-382
    let mut external_output_mode = config.external_output_mode.unwrap_or({
        if screen_mode == ScreenMode::SplitFooter {
            ExternalOutputMode::CaptureStdout
        } else {
            ExternalOutputMode::Passthrough
        }
    });
    if let Some(override_stdout) = env_flag("OTUI_OVERRIDE_STDOUT") {
        external_output_mode = if override_stdout && screen_mode == ScreenMode::SplitFooter {
            ExternalOutputMode::CaptureStdout
        } else {
            ExternalOutputMode::Passthrough
        };
    }

    // renderer.ts:384-386
    if external_output_mode == ExternalOutputMode::CaptureStdout
        && screen_mode != ScreenMode::SplitFooter
    {
        return Err(RendererModeError::CaptureRequiresSplitFooter);
    }

    Ok(ResolvedModes {
        screen_mode,
        footer_height,
        external_output_mode,
    })
}

/// The renderer's global selection — `Selection`
/// (lib/selection.ts:27) reduced to the keyboard-selection state: an
/// anchor pinned relative to its renderable + a focus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererSelection {
    pub anchor_renderable: String,
    /// anchor relative to the renderable's layout position
    /// (`SelectionAnchor`, lib/selection.ts:5-25)
    pub anchor_relative: (i32, i32),
    /// focus in the anchor's coordinate space
    pub focus: (i32, i32),
    pub behavior: SelectionBehavior,
    pub is_start: bool,
    pub is_dragging: bool,
}

/// Terminal cursor state — `Terminal.state.cursor` (terminal.zig) as
/// the renderer sees it: position (1-based), visibility, style, color,
/// plus the emission diff cache (`lastCursor*`, renderer.zig:325, 2968+).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorOutputState {
    pub x: u32,
    pub y: u32,
    pub visible: bool,
    pub style: crate::renderables::textarea::CursorStyle,
    pub blinking: bool,
    pub color: Rgba,
    // diff cache (None = never emitted)
    last_x: Option<u32>,
    last_y: Option<u32>,
    last_visible: Option<bool>,
    last_style_tag: Option<u8>,
    last_blinking: Option<bool>,
    last_color: Option<[u8; 3]>,
}

impl Default for CursorOutputState {
    fn default() -> Self {
        CursorOutputState {
            x: 1,
            y: 1,
            visible: false,
            style: crate::renderables::textarea::CursorStyle::Block,
            blinking: true,
            color: Rgba::rgb(255, 255, 255),
            last_x: None,
            last_y: None,
            last_visible: None,
            last_style_tag: None,
            last_blinking: None,
            last_color: None,
        }
    }
}

impl CursorOutputState {
    /// `Terminal.setCursorPosition` (terminal.zig:1458-1468).
    pub fn set_position(&mut self, x: u32, y: u32, visible: bool) {
        self.x = x.max(1);
        self.y = y.max(1);
        self.visible = visible;
    }

    /// `Terminal.setCursorStyle` + `setCursorColor`.
    pub fn set_style(
        &mut self,
        style: crate::renderables::textarea::CursorStyle,
        blinking: bool,
        color: Rgba,
    ) {
        self.style = style;
        self.blinking = blinking;
        self.color = color;
    }

    /// The cursor emission tail (renderer.zig:2932-3016) — appended to
    /// the frame's SGR stream. `frame_started` mirrors the cell-run
    /// emission state.
    pub(crate) fn emit(&mut self, out: &mut String, frame_started: bool) {
        let style_tag: u8 = match self.style {
            crate::renderables::textarea::CursorStyle::Block => 1,
            crate::renderables::textarea::CursorStyle::Line => 3,
            crate::renderables::textarea::CursorStyle::Underline => 5,
        };
        if self.visible {
            let (cr, cg, cb) = (self.color.red(), self.color.green(), self.color.blue());
            let style_changed =
                self.last_style_tag != Some(style_tag) || self.last_blinking != Some(self.blinking);
            let color_changed = self.last_color != Some([cr, cg, cb]);
            let position_changed = self.last_x != Some(self.x) || self.last_y != Some(self.y);
            let visibility_changed = self.last_visible != Some(true);
            // If any visual output was produced this frame, cursor state
            // must be restored (renderer.zig:2975-2978)
            let needs_restore = frame_started
                || style_changed
                || color_changed
                || position_changed
                || visibility_changed;
            if needs_restore {
                if color_changed {
                    out.push_str(&format!("\x1b]12;#{cr:02x}{cg:02x}{cb:02x}\x07"));
                    self.last_color = Some([cr, cg, cb]);
                }
                if style_changed {
                    // DECSCUSR: 1/2 block, 3/4 line, 5/6 underline (blink/steady)
                    let code = style_tag + u8::from(!self.blinking);
                    out.push_str(&format!("\x1b[{code} q"));
                    self.last_style_tag = Some(style_tag);
                    self.last_blinking = Some(self.blinking);
                }
                out.push_str(&format!("\x1b[{};{}H", self.y, self.x));
                out.push_str("\x1b[?25h");
            }
            self.last_x = Some(self.x);
            self.last_y = Some(self.y);
            self.last_visible = Some(true);
        } else {
            if !frame_started && self.last_visible.is_none_or(|v| v) {
                out.push_str("\x1b[?25l");
            }
            self.last_style_tag = None;
            self.last_blinking = None;
            self.last_color = None;
            self.last_x = None;
            self.last_y = None;
            self.last_visible = Some(false);
        }
    }
}

/// Where output goes — the headless slice has two targets: the cell grid
/// (differential harness) and an in-memory SGR byte stream.
pub enum RenderTarget {
    Headless,
}

/// `KittyKeyboardOptions` (renderer.ts:597-608).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KittyKeyboardOptions {
    /// Disambiguate escape codes. Default: true.
    pub disambiguate: Option<bool>,
    /// Report alternate keys. Default: true.
    pub alternate_keys: Option<bool>,
    /// Report event types (press/repeat/release). Default: false.
    pub events: Option<bool>,
    /// Report all keys as escape codes. Default: false.
    pub all_keys_as_escapes: Option<bool>,
    /// Report text associated with key events. Default: false.
    pub report_text: Option<bool>,
}

// The kitty protocol's progressive-enhancement bits (renderer.ts:584-588)
const KITTY_FLAG_DISAMBIGUATE: u32 = 0b1;
const KITTY_FLAG_EVENT_TYPES: u32 = 0b10;
const KITTY_FLAG_ALTERNATE_KEYS: u32 = 0b100;
const KITTY_FLAG_ALL_KEYS_AS_ESCAPES: u32 = 0b1000;
const KITTY_FLAG_REPORT_TEXT: u32 = 0b10000;

/// `buildKittyKeyboardFlags` (renderer.ts:616-651): disambiguate and
/// alternateKeys default ON (disabled only when explicitly `false`);
/// the rest default OFF (enabled only when explicitly `true`).
/// Returns 0 for `None` (protocol disabled).
#[must_use]
pub fn build_kitty_keyboard_flags(config: Option<&KittyKeyboardOptions>) -> u32 {
    let Some(config) = config else {
        return 0;
    };
    let mut flags = 0u32;
    if config.disambiguate != Some(false) {
        flags |= KITTY_FLAG_DISAMBIGUATE;
    }
    if config.alternate_keys != Some(false) {
        flags |= KITTY_FLAG_ALTERNATE_KEYS;
    }
    if config.events == Some(true) {
        flags |= KITTY_FLAG_EVENT_TYPES;
    }
    if config.all_keys_as_escapes == Some(true) {
        flags |= KITTY_FLAG_ALL_KEYS_AS_ESCAPES;
    }
    if config.report_text == Some(true) {
        flags |= KITTY_FLAG_REPORT_TEXT;
    }
    flags
}

/// `WidthMethod` (native utf8.zig:5-9) — the `caps.unicode` field of
/// [`TerminalCapabilities`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WidthMethod {
    Wcwidth,
    /// zig default (terminal.zig:18).
    #[default]
    Unicode,
    NoZwj,
    UnicodeWide,
}

/// `Terminal.Capabilities` (terminal.zig:13-32) — what the terminal
/// backend's probe state machine accumulates. Defaults are the zig
/// defaults, which double as the "unknown / timed out" snapshot: a
/// 5s probe timeout yields exactly this all-false struct with
/// `unicode = Unicode` (the unknown-fields-default contract of lane
/// A2's `feed_input`).
///
/// `terminal_name` is the one field beyond zig's struct: upstream
/// carries it in `term_info` (terminal.zig:1313-1321 xtversion
/// parse) and the kitty-identity branch reads it back
/// (terminal.zig:1353-1360), so the capability consumer needs it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerminalCapabilities {
    /// terminal.zig:14
    pub kitty_keyboard: bool,
    /// terminal.zig:15 (kitty graphics query — probe byte owned by A3)
    pub kitty_graphics: bool,
    /// terminal.zig:16
    pub rgb: bool,
    /// terminal.zig:17
    pub ansi256: bool,
    /// terminal.zig:18
    pub unicode: WidthMethod,
    /// terminal.zig:19 (DECRPM `1016;2$y`, terminal.zig:1255-1256)
    pub sgr_pixels: bool,
    /// terminal.zig:20 (DECRPM `2031;*$y`, terminal.zig:1259-1261)
    pub color_scheme_updates: bool,
    /// terminal.zig:21 (explicit-width probe family — deferred, see
    /// terminal/mod.rs gap register)
    pub explicit_width: bool,
    /// terminal.zig:22 (same probe family)
    pub scaled_text: bool,
    /// terminal.zig:23 (DA1 attribute `4`, terminal.zig:1122-1140)
    pub sixel: bool,
    /// terminal.zig:24 (DECRPM `1004;*$y`, terminal.zig:1262-1264)
    pub focus_tracking: bool,
    /// terminal.zig:25 (DECRPM `2026;*$y`, terminal.zig:1265-1267)
    pub sync: bool,
    /// terminal.zig:26 (DECRPM `2004;*$y`, terminal.zig:1268-1270)
    pub bracketed_paste: bool,
    /// terminal.zig:27
    pub hyperlinks: bool,
    /// terminal.zig:28
    pub osc52: bool,
    /// terminal.zig:29 (OSC 99 notification protocol — probe bytes
    /// deferred with the notification-query family)
    pub notifications: bool,
    /// terminal.zig:30
    pub explicit_cursor_positioning: bool,
    /// terminal.zig:31
    pub remote: bool,
    /// xtversion identity (terminal.zig:1313-1321, name split per
    /// parseXtversion terminal.zig:1739-1770) — `None` until a
    /// `DCS >|…ST` reply names the terminal.
    pub terminal_name: Option<String>,
    /// The version half of the xtversion reply ("0.40.1" out of
    /// "kitty(0.40.1)", terminal.zig:1747-1766).
    pub terminal_version: Option<String>,
    /// `multiplexer` — env-derived on the TS facade's capabilities
    /// (types.ts:136; terminal.zig:207/655-663, default `.none`).
    /// The tmux value gates the image-protocol fallback to blocks
    /// (renderables/Image.ts:28).
    pub multiplexer: TerminalMultiplexer,
    /// `image_protocol` (types.ts:137; terminal.zig:212, default
    /// `.auto`) — the configured override the image renderable's
    /// `resolveImageRenderProtocol` defers to (Image.ts:26-27).
    pub image_protocol: crate::renderables::image::ImageRenderProtocol,
}

/// `TerminalMultiplexer` (types.ts:136; terminal.zig:196-201) —
/// which multiplexer the session runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TerminalMultiplexer {
    #[default]
    None,
    Tmux,
    Zellij,
    Screen,
}

/// `ThemeMode` (renderer.ts / types.ts:32) — the light/dark reduction
/// of the terminal's OSC 10/11 theme answers. The Rust spelling of
/// upstream's `"dark" | "light"` string union.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    Dark,
    Light,
}

/// `TerminalColors` (terminal-palette.ts:24-36) — the payload shape of
/// the upstream `PALETTE` event (renderer.ts:5187 emits the whole
/// struct). Hex strings, `None` = upstream's `Hex = string | null`
/// null slot. The detector that fills it (TerminalPaletteDetector,
/// terminal-palette.ts:47-52) stays deferred — the variant exists so
/// the event enum reaches upstream shape, but nothing emits it yet.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerminalPalette {
    pub palette: Vec<Option<String>>,
    pub default_foreground: Option<String>,
    pub default_background: Option<String>,
    pub cursor_color: Option<String>,
    pub mouse_foreground: Option<String>,
    pub mouse_background: Option<String>,
    pub tek_foreground: Option<String>,
    pub tek_background: Option<String>,
    pub highlight_background: Option<String>,
    pub highlight_foreground: Option<String>,
}
