//! The clipboard family — port of `packages/core/src/lib/clipboard.ts`
//! (`createClipboard` :179, `createRendererClipboardAdapter` :308,
//! `ClipboardTarget` :292) plus the host-service half from
//! `host-clipboard.internal.ts` (`createHostClipboardWithBackend` :176,
//! `validateClipboardText` :116, option/MIME normalization).
//!
//! **Sync model.** Upstream is promise-based with `AbortSignal`
//! cancellation; this port is synchronous (the crate has no async
//! runtime — see ARCHITECTURE.md). The cancellation contract maps to a
//! shared flag ([`AbortSignal`]): the pre-abort fast paths and the
//! mid-flight abort gates (`!signal.aborted` at clipboard.ts:216) are
//! observable exactly as upstream; test backends can flip the flag
//! during a call to pin the "aborted while the host worked" paths.
//! Promise-lifetime behavior (dispose waits for in-flight operations)
//! is degenerate — a synchronous call cannot be in flight at rest — so
//! `dispose` reduces to: mark disposed, dispose the host (once).
//!
//! The native backend (`host-clipboard.native.ts` over the zig render
//! lib) is out of scope: hosts are injected as
//! [`HostClipboardBackend`] implementations (upstream's seam for this
//! exact purpose — `createHostClipboardWithBackend`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// cancellation (the AbortSignal slice the clipboard reads)
// ---------------------------------------------------------------------------

/// The `AbortSignal` slice the clipboard observes: `signal.aborted`
/// reads (clipboard.ts:216, 242; host-clipboard.internal.ts:196, 219,
/// 232). Shared so a caller (or a test backend) can abort mid-flight.
#[derive(Debug, Clone, Default)]
pub struct AbortSignal {
    flag: Arc<AtomicBool>,
}

impl AbortSignal {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `signal.aborted`.
    #[must_use]
    pub fn aborted(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// `controller.abort()`.
    pub fn abort(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// errors (upstream rejects with TypeError / RangeError / Error)
// ---------------------------------------------------------------------------

/// Which JS error class a rejection carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardErrorKind {
    /// `new TypeError(...)`
    Type,
    /// `new RangeError(...)`
    Range,
    /// `new Error(...)`
    Error,
}

/// A rejected promise — kind + the exact upstream message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardError {
    pub kind: ClipboardErrorKind,
    pub message: String,
}

impl ClipboardError {
    fn ty(message: impl Into<String>) -> Self {
        ClipboardError {
            kind: ClipboardErrorKind::Type,
            message: message.into(),
        }
    }

    fn range(message: impl Into<String>) -> Self {
        ClipboardError {
            kind: ClipboardErrorKind::Range,
            message: message.into(),
        }
    }

    fn error(message: impl Into<String>) -> Self {
        ClipboardError {
            kind: ClipboardErrorKind::Error,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind_str(), self.message)
    }
}

impl std::error::Error for ClipboardError {}

impl ClipboardError {
    /// The JS class name (`TypeError`/`RangeError`/`Error`).
    #[must_use]
    pub fn kind_str(&self) -> &'static str {
        match self.kind {
            ClipboardErrorKind::Type => "TypeError",
            ClipboardErrorKind::Range => "RangeError",
            ClipboardErrorKind::Error => "Error",
        }
    }
}

// ---------------------------------------------------------------------------
// shared vocabulary (clipboard.ts:13-154)
// ---------------------------------------------------------------------------

/// `ClipboardRepresentation` (clipboard.ts:13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardRepresentation {
    /// `mimeType` — canonical lowercase MIME essence, no parameters.
    pub mime_type: String,
    /// caller-owned bytes; the backend does not reuse or mutate them.
    pub bytes: Vec<u8>,
}

/// `ClipboardSelection` (clipboard.ts:20): `"clipboard" | "primary"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardSelection {
    Clipboard,
    Primary,
}

impl ClipboardSelection {
    /// `validateSelection` (clipboard.ts:156-162): default `"clipboard"`,
    /// `TypeError("selection must be clipboard or primary")` otherwise.
    pub fn parse(selection: Option<&str>) -> Result<Self, ClipboardError> {
        match selection {
            None | Some("clipboard") => Ok(ClipboardSelection::Clipboard),
            Some("primary") => Ok(ClipboardSelection::Primary),
            Some(_) => Err(ClipboardError::ty("selection must be clipboard or primary")),
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            ClipboardSelection::Clipboard => "clipboard",
            ClipboardSelection::Primary => "primary",
        }
    }
}

/// `ClipboardReadOptions` (clipboard.ts:22).
#[derive(Debug, Clone, Default)]
pub struct ClipboardReadOptions {
    /// `preferredTypes` — validated non-empty by the host half.
    pub preferred_types: Vec<String>,
    pub selection: Option<ClipboardSelection>,
    pub signal: Option<AbortSignal>,
}

/// `ClipboardReadResult` (clipboard.ts:32-35).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardReadResult {
    Read(ClipboardRepresentation),
    Empty,
    Unsupported,
    Cancelled,
    TimedOut,
    LimitExceeded,
    Failed { error: String },
}

impl ClipboardReadResult {
    /// The `status` string of the union member.
    #[must_use]
    pub fn status_str(&self) -> &'static str {
        match self {
            ClipboardReadResult::Read(_) => "read",
            ClipboardReadResult::Empty => "empty",
            ClipboardReadResult::Unsupported => "unsupported",
            ClipboardReadResult::Cancelled => "cancelled",
            ClipboardReadResult::TimedOut => "timed-out",
            ClipboardReadResult::LimitExceeded => "limit-exceeded",
            ClipboardReadResult::Failed { .. } => "failed",
        }
    }
}

/// `HostClipboardWriteResult` (clipboard.ts:53-55) — the
/// `{ status: "not-attempted" }` member of `ClipboardWriteResult.host`
/// (clipboard.ts:93) folds in as [`Self::NotAttempted`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostClipboardWriteResult {
    Written,
    Unsupported,
    Cancelled,
    TimedOut,
    Failed { error: String },
    NotAttempted,
}

impl HostClipboardWriteResult {
    #[must_use]
    pub fn status_str(&self) -> &'static str {
        match self {
            HostClipboardWriteResult::Written => "written",
            HostClipboardWriteResult::Unsupported => "unsupported",
            HostClipboardWriteResult::Cancelled => "cancelled",
            HostClipboardWriteResult::TimedOut => "timed-out",
            HostClipboardWriteResult::Failed { .. } => "failed",
            HostClipboardWriteResult::NotAttempted => "not-attempted",
        }
    }
}

/// `HostClipboardClearResult` (clipboard.ts:58-60) + the
/// `not-attempted` fold-in (clipboard.ts:98).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostClipboardClearResult {
    Cleared,
    Unsupported,
    Cancelled,
    TimedOut,
    Failed { error: String },
    NotAttempted,
}

impl HostClipboardClearResult {
    #[must_use]
    pub fn status_str(&self) -> &'static str {
        match self {
            HostClipboardClearResult::Cleared => "cleared",
            HostClipboardClearResult::Unsupported => "unsupported",
            HostClipboardClearResult::Cancelled => "cancelled",
            HostClipboardClearResult::TimedOut => "timed-out",
            HostClipboardClearResult::Failed { .. } => "failed",
            HostClipboardClearResult::NotAttempted => "not-attempted",
        }
    }
}

/// `HostClipboardReadOptions` (clipboard.ts:37) — what a backend `read`
/// receives (all fields normalized).
#[derive(Debug, Clone)]
pub struct HostClipboardReadOptions {
    pub preferred_types: Vec<String>,
    pub selection: ClipboardSelection,
    pub max_bytes: u32,
    pub timeout_ms: u32,
    pub signal: AbortSignal,
}

/// `HostClipboardWriteOptions` (clipboard.ts:47).
#[derive(Debug, Clone)]
pub struct HostClipboardWriteOptions {
    pub selection: ClipboardSelection,
    pub timeout_ms: u32,
    pub signal: AbortSignal,
}

/// `HostClipboardBackend` (clipboard.ts:62-70) — the host seam.
/// Upstream's native backend polls a zig service asynchronously; the
/// sync port calls through directly.
pub trait HostClipboardBackend {
    /// Returns the first usable representation from `preferred_types`.
    fn read(&mut self, options: &HostClipboardReadOptions) -> ClipboardReadResult;
    /// Writes validated, nonempty text without NUL characters.
    fn write_text(
        &mut self,
        text: &str,
        options: &HostClipboardWriteOptions,
    ) -> HostClipboardWriteResult;
    /// Clears the selection with the platform's clear operation.
    fn clear(&mut self, options: &HostClipboardWriteOptions) -> HostClipboardClearResult;
    fn dispose(&mut self);
}

/// `ClipboardWriteDestination` (clipboard.ts:73).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardWriteDestination {
    TerminalOnly,
    HostOnly,
    BestAvailable,
    AllAvailable,
}

impl ClipboardWriteDestination {
    /// `validateDestination` (clipboard.ts:168-177) — the
    /// `TypeError("destination is not a supported clipboard policy")`
    /// path lives at the string→enum boundary.
    pub fn parse(destination: &str) -> Result<Self, ClipboardError> {
        match destination {
            "terminal-only" => Ok(ClipboardWriteDestination::TerminalOnly),
            "host-only" => Ok(ClipboardWriteDestination::HostOnly),
            "best-available" => Ok(ClipboardWriteDestination::BestAvailable),
            "all-available" => Ok(ClipboardWriteDestination::AllAvailable),
            _ => Err(ClipboardError::ty(
                "destination is not a supported clipboard policy",
            )),
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            ClipboardWriteDestination::TerminalOnly => "terminal-only",
            ClipboardWriteDestination::HostOnly => "host-only",
            ClipboardWriteDestination::BestAvailable => "best-available",
            ClipboardWriteDestination::AllAvailable => "all-available",
        }
    }
}

/// `ClipboardWriteOptions` (clipboard.ts:75).
#[derive(Debug, Clone, Default)]
pub struct ClipboardWriteOptions {
    pub destination: Option<ClipboardWriteDestination>,
    pub selection: Option<ClipboardSelection>,
    /// `allowRemoteHost` (clipboard.ts:80).
    pub allow_remote_host: bool,
    pub signal: Option<AbortSignal>,
}

/// The `capability` of `TerminalClipboardOperationResult`
/// (clipboard.ts:89) — `"supported" | "unsupported" | "unknown"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Osc52Support {
    Supported,
    Unsupported,
    Unknown,
}

impl Osc52Support {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Osc52Support::Supported => "supported",
            Osc52Support::Unsupported => "unsupported",
            Osc52Support::Unknown => "unknown",
        }
    }
}

/// `TerminalClipboardOperationResult` (clipboard.ts:85-90).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalClipboardOperationResult {
    /// `"attempted" | "local-failure" | "not-attempted"`.
    pub attempted: Option<bool>,
    pub capability: Osc52Support,
}

impl TerminalClipboardOperationResult {
    /// The `status` string (`attempted`/`local-failure`/`not-attempted`).
    #[must_use]
    pub fn status_str(&self) -> &'static str {
        match self.attempted {
            Some(true) => "attempted",
            Some(false) => "local-failure",
            None => "not-attempted",
        }
    }
}

/// `NOT_ATTEMPTED_TERMINAL` (clipboard.ts:151-154).
const NOT_ATTEMPTED_TERMINAL: TerminalClipboardOperationResult = TerminalClipboardOperationResult {
    attempted: None,
    capability: Osc52Support::Unknown,
};

/// `ClipboardWriteResult` (clipboard.ts:92).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardWriteResult {
    pub host: HostClipboardWriteResult,
    pub terminal: TerminalClipboardOperationResult,
}

/// `ClipboardClearResult` (clipboard.ts:97).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardClearResult {
    pub host: HostClipboardClearResult,
    pub terminal: TerminalClipboardOperationResult,
}

/// `TerminalClipboardAdapter` (clipboard.ts:140-144).
pub trait TerminalClipboardAdapter {
    /// `remote` getter.
    fn remote(&self) -> bool;
    fn write_text(
        &mut self,
        text: &str,
        selection: ClipboardSelection,
    ) -> TerminalClipboardOperationResult;
    fn clear(&mut self, selection: ClipboardSelection) -> TerminalClipboardOperationResult;
}

/// `HostClipboardOperationOptions` (clipboard.ts:113).
#[derive(Debug, Clone, Default)]
pub struct HostClipboardOperationOptions {
    pub selection: Option<ClipboardSelection>,
    pub signal: Option<AbortSignal>,
}

// ---------------------------------------------------------------------------
// host service — host-clipboard.internal.ts
// ---------------------------------------------------------------------------

/// `DEFAULT_CLIPBOARD_TIMEOUT_MS` (internal :9).
pub const DEFAULT_CLIPBOARD_TIMEOUT_MS: f64 = 1_000.0;
/// `DEFAULT_CLIPBOARD_MAX_BYTES` (internal :10).
pub const DEFAULT_CLIPBOARD_MAX_BYTES: f64 = (8 * 1024 * 1024) as f64;
/// `DEFAULT_CLIPBOARD_MAX_IMAGE_PIXELS` (internal :11).
pub const DEFAULT_CLIPBOARD_MAX_IMAGE_PIXELS: f64 = (64 * 1024 * 1024) as f64;
/// `DEFAULT_CLIPBOARD_MAX_CONVERSION_BYTES` (internal :12).
pub const DEFAULT_CLIPBOARD_MAX_CONVERSION_BYTES: f64 = (512 * 1024 * 1024) as f64;
/// `DEFAULT_CLIPBOARD_MAX_CONCURRENT_OPERATIONS` (internal :13).
pub const DEFAULT_CLIPBOARD_MAX_CONCURRENT_OPERATIONS: f64 = 16.0;
/// `DEFAULT_CLIPBOARD_MAX_PROVIDER_TRANSFERS` (internal :14).
pub const DEFAULT_CLIPBOARD_MAX_PROVIDER_TRANSFERS: f64 = 16.0;
/// `MAX_U32` (internal :15).
pub const MAX_U32: f64 = 4_294_967_295.0;
/// `HOST_CLIPBOARD_MIME_PREFERENCE_COUNT_MAX` (internal :17).
pub const HOST_CLIPBOARD_MIME_PREFERENCE_COUNT_MAX: usize = 64;
/// `HOST_CLIPBOARD_MIME_ESSENCE_BYTES_MAX` (internal :18).
pub const HOST_CLIPBOARD_MIME_ESSENCE_BYTES_MAX: usize = 255;

/// `HostClipboardOptions` (clipboard.ts:128) — `f64` fields mirror the
/// upstream `number` inputs so validation rejects non-integers
/// (`validateU32`, internal :39-44).
#[derive(Debug, Clone, Default)]
pub struct HostClipboardOptions {
    pub timeout_ms: Option<f64>,
    pub max_read_bytes: Option<f64>,
    pub max_write_bytes: Option<f64>,
    pub max_image_pixels: Option<f64>,
    pub max_conversion_bytes: Option<f64>,
    pub max_concurrent_operations: Option<f64>,
    pub max_provider_transfers: Option<f64>,
    pub wayland_seat: Option<String>,
}

/// `NormalizedHostClipboardOptions` (internal :20-29).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedHostClipboardOptions {
    pub timeout_ms: u32,
    pub max_read_bytes: u32,
    pub max_write_bytes: u32,
    pub max_image_pixels: u32,
    pub max_conversion_bytes: u32,
    pub max_concurrent_operations: u32,
    pub max_provider_transfers: u32,
    pub wayland_seat: Option<String>,
}

/// `validateU32` (internal :39-44): integer in `[0, MAX_U32]`.
fn validate_u32(name: &str, value: f64) -> Result<u32, ClipboardError> {
    if !value.is_finite() || value.fract() != 0.0 || value < 0.0 || value > MAX_U32 {
        return Err(ClipboardError::range(format!(
            "{name} must be an integer from 0 through 4294967295"
        )));
    }
    Ok(value as u32)
}

/// `validatePositiveU32` (internal :46-50).
fn validate_positive_u32(name: &str, value: f64) -> Result<u32, ClipboardError> {
    let validated = validate_u32(name, value)?;
    if validated == 0 {
        return Err(ClipboardError::range(format!(
            "{name} must be greater than zero"
        )));
    }
    Ok(validated)
}

/// `normalizeOptions` (internal :52-79).
fn normalize_options(
    options: &HostClipboardOptions,
) -> Result<NormalizedHostClipboardOptions, ClipboardError> {
    if let Some(seat) = &options.wayland_seat {
        if seat.is_empty() || seat.contains('\0') {
            return Err(ClipboardError::ty(
                "waylandSeat must be a non-empty string without NUL characters",
            ));
        }
    }
    Ok(NormalizedHostClipboardOptions {
        timeout_ms: validate_u32(
            "timeoutMs",
            options.timeout_ms.unwrap_or(DEFAULT_CLIPBOARD_TIMEOUT_MS),
        )?,
        max_read_bytes: validate_u32(
            "maxReadBytes",
            options
                .max_read_bytes
                .unwrap_or(DEFAULT_CLIPBOARD_MAX_BYTES),
        )?,
        max_write_bytes: validate_u32(
            "maxWriteBytes",
            options
                .max_write_bytes
                .unwrap_or(DEFAULT_CLIPBOARD_MAX_BYTES),
        )?,
        max_image_pixels: validate_u32(
            "maxImagePixels",
            options
                .max_image_pixels
                .unwrap_or(DEFAULT_CLIPBOARD_MAX_IMAGE_PIXELS),
        )?,
        max_conversion_bytes: validate_u32(
            "maxConversionBytes",
            options
                .max_conversion_bytes
                .unwrap_or(DEFAULT_CLIPBOARD_MAX_CONVERSION_BYTES),
        )?,
        max_concurrent_operations: validate_positive_u32(
            "maxConcurrentOperations",
            options
                .max_concurrent_operations
                .unwrap_or(DEFAULT_CLIPBOARD_MAX_CONCURRENT_OPERATIONS),
        )?,
        max_provider_transfers: validate_positive_u32(
            "maxProviderTransfers",
            options
                .max_provider_transfers
                .unwrap_or(DEFAULT_CLIPBOARD_MAX_PROVIDER_TRANSFERS),
        )?,
        wayland_seat: options.wayland_seat.clone(),
    })
}

/// `MIME_ESSENCE_PATTERN` (internal :16):
/// `/^[a-z0-9!#$%&'*+.^_`|~-]+\/[a-z0-9!#$%&'*+.^_`|~-]+$/i` —
/// case-insensitive (the `i` flag; normalization lowercases after).
fn is_mime_essence(mime_type: &str) -> bool {
    let mut parts = mime_type.split('/');
    let (Some(typ), Some(sub), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let token_ok = |t: &str| {
        !t.is_empty()
            && t.bytes().all(|b| {
                b.is_ascii_alphanumeric()
                    || matches!(
                        b,
                        b'!' | b'#'
                            | b'$'
                            | b'%'
                            | b'&'
                            | b'\''
                            | b'*'
                            | b'+'
                            | b'.'
                            | b'^'
                            | b'_'
                            | b'`'
                            | b'|'
                            | b'~'
                            | b'-'
                    )
            })
    };
    // valid MIME essence tokens are ASCII, so code units equal bytes
    token_ok(typ) && token_ok(sub)
}

/// `normalizePreferredTypes` (internal :81-106).
fn normalize_preferred_types(preferred_types: &[String]) -> Result<Vec<String>, ClipboardError> {
    if preferred_types.is_empty() {
        return Err(ClipboardError::ty(
            "preferredTypes must contain at least one MIME essence type",
        ));
    }
    if preferred_types.len() > HOST_CLIPBOARD_MIME_PREFERENCE_COUNT_MAX {
        return Err(ClipboardError::range(format!(
            "preferredTypes must contain at most {HOST_CLIPBOARD_MIME_PREFERENCE_COUNT_MAX} MIME essence types"
        )));
    }
    let mut normalized = Vec::with_capacity(preferred_types.len());
    for mime_type in preferred_types {
        if mime_type.len() > HOST_CLIPBOARD_MIME_ESSENCE_BYTES_MAX {
            return Err(ClipboardError::range(format!(
                "preferredTypes MIME essences must be at most {HOST_CLIPBOARD_MIME_ESSENCE_BYTES_MAX} ASCII bytes"
            )));
        }
        if !is_mime_essence(mime_type) {
            return Err(ClipboardError::ty(
                "preferredTypes must contain valid MIME essence types without parameters",
            ));
        }
        normalized.push(mime_type.to_lowercase());
    }
    Ok(normalized)
}

/// `validateClipboardText` (internal :116-139): non-empty, no NUL,
/// UTF-8 bytes ≤ `max_write_bytes`. The unpaired-surrogate check is
/// UTF-16-specific and unrepresentable in a Rust `&str`.
pub fn validate_clipboard_text(text: &str, max_write_bytes: u32) -> Result<(), ClipboardError> {
    if text.is_empty() {
        return Err(ClipboardError::ty("writeText requires non-empty text"));
    }
    if text.contains('\0') {
        return Err(ClipboardError::ty(
            "writeText does not support NUL characters",
        ));
    }
    let byte_limit = (max_write_bytes as f64).min(MAX_U32) as u64;
    let mut byte_length = 0u64;
    for character in text.chars() {
        byte_length += character.len_utf8() as u64;
        if byte_length > byte_limit {
            return Err(ClipboardError::range(format!(
                "writeText exceeds the configured {max_write_bytes} byte limit"
            )));
        }
    }
    Ok(())
}

/// `HostClipboardService` (clipboard.ts:119-126) — the service half of
/// `createHostClipboardWithBackend` (internal :176-252).
pub struct HostClipboardService {
    backend: Box<dyn HostClipboardBackend>,
    config: NormalizedHostClipboardOptions,
    disposed: bool,
}

impl std::fmt::Debug for HostClipboardService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostClipboardService")
            .field("config", &self.config)
            .field("disposed", &self.disposed)
            .finish_non_exhaustive()
    }
}

impl HostClipboardService {
    /// `maxWriteBytes` (internal :190).
    #[must_use]
    pub fn max_write_bytes(&self) -> u32 {
        self.config.max_write_bytes
    }

    fn assert_usable(&self) -> Result<(), ClipboardError> {
        if self.disposed {
            return Err(ClipboardError::error("Host clipboard service is disposed"));
        }
        Ok(())
    }

    /// `read` (internal :191-213): normalize → pre-abort → timeout-0 →
    /// backend, with the `maxReadBytes` re-check on the way out
    /// (internal :207).
    pub fn read(
        &mut self,
        options: &ClipboardReadOptions,
    ) -> Result<ClipboardReadResult, ClipboardError> {
        self.assert_usable()?;
        let preferred_types = normalize_preferred_types(&options.preferred_types)?;
        let selection = options.selection.unwrap_or(ClipboardSelection::Clipboard);
        let signal = options.signal.clone().unwrap_or_default();
        if signal.aborted() {
            return Ok(ClipboardReadResult::Cancelled);
        }
        if self.config.timeout_ms == 0 {
            return Ok(ClipboardReadResult::TimedOut);
        }
        let result = self.backend.read(&HostClipboardReadOptions {
            preferred_types,
            selection,
            max_bytes: self.config.max_read_bytes,
            timeout_ms: self.config.timeout_ms,
            signal,
        });
        Ok(match result {
            ClipboardReadResult::Read(representation) => {
                if representation.bytes.len() as u64 > u64::from(self.config.max_read_bytes) {
                    ClipboardReadResult::LimitExceeded
                } else {
                    ClipboardReadResult::Read(representation)
                }
            }
            other => other,
        })
    }

    /// `writeText` (internal :214-227).
    pub fn write_text(
        &mut self,
        text: &str,
        options: &HostClipboardOperationOptions,
    ) -> Result<HostClipboardWriteResult, ClipboardError> {
        self.assert_usable()?;
        validate_clipboard_text(text, self.config.max_write_bytes)?;
        let selection = options.selection.unwrap_or(ClipboardSelection::Clipboard);
        let signal = options.signal.clone().unwrap_or_default();
        if signal.aborted() {
            return Ok(HostClipboardWriteResult::Cancelled);
        }
        if self.config.timeout_ms == 0 {
            return Ok(HostClipboardWriteResult::TimedOut);
        }
        Ok(self.backend.write_text(
            text,
            &HostClipboardWriteOptions {
                selection,
                timeout_ms: self.config.timeout_ms,
                signal,
            },
        ))
    }

    /// `clear` (internal :228-240).
    pub fn clear(
        &mut self,
        options: &HostClipboardOperationOptions,
    ) -> Result<HostClipboardClearResult, ClipboardError> {
        self.assert_usable()?;
        let selection = options.selection.unwrap_or(ClipboardSelection::Clipboard);
        let signal = options.signal.clone().unwrap_or_default();
        if signal.aborted() {
            return Ok(HostClipboardClearResult::Cancelled);
        }
        if self.config.timeout_ms == 0 {
            return Ok(HostClipboardClearResult::TimedOut);
        }
        Ok(self.backend.clear(&HostClipboardWriteOptions {
            selection,
            timeout_ms: self.config.timeout_ms,
            signal,
        }))
    }

    /// `dispose` (internal :241-250) — sync: dispose the backend once.
    pub fn dispose(&mut self) {
        if self.disposed {
            return;
        }
        self.disposed = true;
        self.backend.dispose();
    }
}

/// `createHostClipboardWithBackend` (internal :176). Options normalize
/// (and validate) before the backend factory runs — pinned by
/// host-clipboard.test.ts:105-125.
pub fn create_host_clipboard_with_backend(
    options: HostClipboardOptions,
    backend: Box<dyn HostClipboardBackend>,
) -> Result<HostClipboardService, ClipboardError> {
    let config = normalize_options(&options)?;
    Ok(HostClipboardService {
        backend,
        config,
        disposed: false,
    })
}

// ---------------------------------------------------------------------------
// createClipboard — clipboard.ts:179-290
// ---------------------------------------------------------------------------

/// `ClipboardOptions` (clipboard.ts:146).
pub struct ClipboardOptions {
    pub host: HostClipboardService,
    pub terminal: Box<dyn TerminalClipboardAdapter>,
}

/// `ClipboardService` (clipboard.ts:102-111) — the composed service
/// from `createClipboard` (clipboard.ts:179).
pub struct ClipboardService {
    host: HostClipboardService,
    terminal: Box<dyn TerminalClipboardAdapter>,
    disposed: bool,
}

impl ClipboardService {
    /// `canUseRemoteHost` (clipboard.ts:188-189).
    fn can_use_remote_host(&self, options: &ClipboardWriteOptions) -> bool {
        !self.terminal.remote() || options.allow_remote_host
    }

    fn assert_usable(&self) -> Result<(), ClipboardError> {
        if self.disposed {
            return Err(ClipboardError::error("Clipboard service is disposed"));
        }
        Ok(())
    }

    /// `composeMutation` (clipboard.ts:197-226) for writes. Sync note:
    /// upstream dispatches the terminal op while the host promise is
    /// pending (clipboard.ts:221-224); the sync port runs the host to
    /// completion first — the event order (`host-write` then
    /// `terminal-write`) is identical because upstream's host promise
    /// runs its synchronous prefix before the terminal dispatch.
    fn compose_write(
        &mut self,
        text: &str,
        options: &ClipboardWriteOptions,
        destination: ClipboardWriteDestination,
        selection: ClipboardSelection,
        signal: &AbortSignal,
    ) -> ClipboardWriteResult {
        let host_write = |host: &mut HostClipboardService| {
            host.write_text(text, &operation_options(selection, signal))
                .unwrap_or(HostClipboardWriteResult::Failed {
                    error: String::new(),
                })
        };
        match destination {
            ClipboardWriteDestination::TerminalOnly => ClipboardWriteResult {
                host: HostClipboardWriteResult::NotAttempted,
                terminal: self.terminal.write_text(text, selection),
            },
            ClipboardWriteDestination::HostOnly => {
                let host = if self.can_use_remote_host(options) {
                    host_write(&mut self.host)
                } else {
                    HostClipboardWriteResult::NotAttempted
                };
                ClipboardWriteResult {
                    host,
                    terminal: NOT_ATTEMPTED_TERMINAL,
                }
            }
            ClipboardWriteDestination::BestAvailable => {
                if self.terminal.remote() {
                    return ClipboardWriteResult {
                        host: HostClipboardWriteResult::NotAttempted,
                        terminal: self.terminal.write_text(text, selection),
                    };
                }
                let host_result = host_write(&mut self.host);
                let terminal = if !signal.aborted()
                    && (host_result == HostClipboardWriteResult::Unsupported
                        || matches!(host_result, HostClipboardWriteResult::Failed { .. }))
                {
                    self.terminal.write_text(text, selection)
                } else {
                    NOT_ATTEMPTED_TERMINAL
                };
                ClipboardWriteResult {
                    host: host_result,
                    terminal,
                }
            }
            ClipboardWriteDestination::AllAvailable => {
                let host = if self.can_use_remote_host(options) {
                    host_write(&mut self.host)
                } else {
                    HostClipboardWriteResult::NotAttempted
                };
                let terminal = self.terminal.write_text(text, selection);
                ClipboardWriteResult { host, terminal }
            }
        }
    }

    /// `composeMutation` for clears (the same policy table over the
    /// clear operations, clipboard.ts:258-278).
    fn compose_clear(
        &mut self,
        options: &ClipboardWriteOptions,
        destination: ClipboardWriteDestination,
        selection: ClipboardSelection,
        signal: &AbortSignal,
    ) -> ClipboardClearResult {
        let host_clear = |host: &mut HostClipboardService| {
            host.clear(&operation_options(selection, signal)).unwrap_or(
                HostClipboardClearResult::Failed {
                    error: String::new(),
                },
            )
        };
        match destination {
            ClipboardWriteDestination::TerminalOnly => ClipboardClearResult {
                host: HostClipboardClearResult::NotAttempted,
                terminal: self.terminal.clear(selection),
            },
            ClipboardWriteDestination::HostOnly => {
                let host = if self.can_use_remote_host(options) {
                    host_clear(&mut self.host)
                } else {
                    HostClipboardClearResult::NotAttempted
                };
                ClipboardClearResult {
                    host,
                    terminal: NOT_ATTEMPTED_TERMINAL,
                }
            }
            ClipboardWriteDestination::BestAvailable => {
                if self.terminal.remote() {
                    return ClipboardClearResult {
                        host: HostClipboardClearResult::NotAttempted,
                        terminal: self.terminal.clear(selection),
                    };
                }
                let host_result = host_clear(&mut self.host);
                let terminal = if !signal.aborted()
                    && (host_result == HostClipboardClearResult::Unsupported
                        || matches!(host_result, HostClipboardClearResult::Failed { .. }))
                {
                    self.terminal.clear(selection)
                } else {
                    NOT_ATTEMPTED_TERMINAL
                };
                ClipboardClearResult {
                    host: host_result,
                    terminal,
                }
            }
            ClipboardWriteDestination::AllAvailable => {
                let host = if self.can_use_remote_host(options) {
                    host_clear(&mut self.host)
                } else {
                    HostClipboardClearResult::NotAttempted
                };
                let terminal = self.terminal.clear(selection);
                ClipboardClearResult { host, terminal }
            }
        }
    }

    /// `read` (clipboard.ts:228-235): passthrough to the host (the host
    /// validates `preferredTypes`/`selection`).
    pub fn read(
        &mut self,
        options: &ClipboardReadOptions,
    ) -> Result<ClipboardReadResult, ClipboardError> {
        self.assert_usable()?;
        self.host.read(options)
    }

    /// `writeText` (clipboard.ts:236-257). Validation order: usable →
    /// destination → text → selection → pre-abort fast path.
    pub fn write_text(
        &mut self,
        text: &str,
        options: &ClipboardWriteOptions,
    ) -> Result<ClipboardWriteResult, ClipboardError> {
        self.assert_usable()?;
        let destination = options
            .destination
            .ok_or_else(|| ClipboardError::ty("destination is not a supported clipboard policy"))?;
        validate_clipboard_text(text, self.host.max_write_bytes())?;
        let selection = options.selection.unwrap_or(ClipboardSelection::Clipboard);
        if let Some(signal) = &options.signal {
            if signal.aborted() {
                return Ok(ClipboardWriteResult {
                    host: HostClipboardWriteResult::NotAttempted,
                    terminal: NOT_ATTEMPTED_TERMINAL,
                });
            }
        }
        let signal = options.signal.clone().unwrap_or_default();
        Ok(self.compose_write(text, options, destination, selection, &signal))
    }

    /// `clear` (clipboard.ts:258-278).
    pub fn clear(
        &mut self,
        options: &ClipboardWriteOptions,
    ) -> Result<ClipboardClearResult, ClipboardError> {
        self.assert_usable()?;
        let destination = options
            .destination
            .ok_or_else(|| ClipboardError::ty("destination is not a supported clipboard policy"))?;
        let selection = options.selection.unwrap_or(ClipboardSelection::Clipboard);
        if let Some(signal) = &options.signal {
            if signal.aborted() {
                return Ok(ClipboardClearResult {
                    host: HostClipboardClearResult::NotAttempted,
                    terminal: NOT_ATTEMPTED_TERMINAL,
                });
            }
        }
        let signal = options.signal.clone().unwrap_or_default();
        Ok(self.compose_clear(options, destination, selection, &signal))
    }

    /// `dispose` (clipboard.ts:279-288): idempotent; upstream aborts
    /// active operations and waits for them before disposing the host —
    /// in the sync port no operation can be in flight at rest, so this
    /// reduces to dispose-the-host-once.
    pub fn dispose(&mut self) {
        if self.disposed {
            return;
        }
        self.disposed = true;
        self.host.dispose();
    }
}

/// `createClipboard` (clipboard.ts:179).
pub fn create_clipboard(options: ClipboardOptions) -> ClipboardService {
    ClipboardService {
        host: options.host,
        terminal: options.terminal,
        disposed: false,
    }
}

// helper keeping the compose closures close to upstream's
// `operationOptions` shape (clipboard.ts:246-247)
fn operation_options(
    selection: ClipboardSelection,
    signal: &AbortSignal,
) -> HostClipboardOperationOptions {
    HostClipboardOperationOptions {
        selection: Some(selection),
        signal: Some(signal.clone()),
    }
}

// ---------------------------------------------------------------------------
// createRendererClipboardAdapter — clipboard.ts:292-335
// ---------------------------------------------------------------------------

/// `ClipboardTarget` (clipboard.ts:292-297) — the native selection
/// target ABI (pinned: clipboard.test.ts:19-24).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardTarget {
    Clipboard = 0,
    Primary = 1,
    Select = 2,
    Secondary = 3,
}

impl ClipboardTarget {
    /// The numeric value the native ABI receives.
    #[must_use]
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

/// The `capabilities` field of `RendererClipboardBoundary`
/// (clipboard.ts:300-303).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundaryCapabilities {
    pub remote: bool,
    pub osc52_support: Osc52Support,
}

/// `RendererClipboardBoundary` (clipboard.ts:299-306) — implemented
/// upstream by `CliRenderer` (renderer.ts:4125-4134).
pub trait RendererClipboardBoundary {
    fn capabilities(&self) -> Option<BoundaryCapabilities>;
    /// `copyToClipboardOSC52(text, target?)` — `true` = dispatched.
    fn copy_to_clipboard_osc52(&mut self, text: &str, target: Option<ClipboardTarget>) -> bool;
    /// `clearClipboardOSC52(target?)`.
    fn clear_clipboard_osc52(&mut self, target: Option<ClipboardTarget>) -> bool;
}

/// The adapter `createRendererClipboardAdapter` returns.
pub struct RendererClipboardAdapter {
    boundary: Box<dyn RendererClipboardBoundary>,
}

impl RendererClipboardAdapter {
    /// `targetFor` (clipboard.ts:309-310).
    fn target_for(selection: ClipboardSelection) -> ClipboardTarget {
        match selection {
            ClipboardSelection::Primary => ClipboardTarget::Primary,
            ClipboardSelection::Clipboard => ClipboardTarget::Clipboard,
        }
    }

    /// `capability()` (clipboard.ts:311-312).
    fn capability(&self) -> Osc52Support {
        self.boundary
            .capabilities()
            .map_or(Osc52Support::Unknown, |c| c.osc52_support)
    }
}

impl TerminalClipboardAdapter for RendererClipboardAdapter {
    /// `get remote()` (clipboard.ts:315-317): `capabilities?.remote ?? true`.
    fn remote(&self) -> bool {
        self.boundary.capabilities().is_none_or(|c| c.remote)
    }

    /// `writeText` (clipboard.ts:318-325).
    fn write_text(
        &mut self,
        text: &str,
        selection: ClipboardSelection,
    ) -> TerminalClipboardOperationResult {
        let current_capability = self.capability();
        if current_capability == Osc52Support::Unsupported {
            return TerminalClipboardOperationResult {
                attempted: None,
                capability: current_capability,
            };
        }
        TerminalClipboardOperationResult {
            attempted: Some(
                self.boundary
                    .copy_to_clipboard_osc52(text, Some(Self::target_for(selection))),
            ),
            capability: current_capability,
        }
    }

    /// `clear` (clipboard.ts:326-333).
    fn clear(&mut self, selection: ClipboardSelection) -> TerminalClipboardOperationResult {
        let current_capability = self.capability();
        if current_capability == Osc52Support::Unsupported {
            return TerminalClipboardOperationResult {
                attempted: None,
                capability: current_capability,
            };
        }
        TerminalClipboardOperationResult {
            attempted: Some(
                self.boundary
                    .clear_clipboard_osc52(Some(Self::target_for(selection))),
            ),
            capability: current_capability,
        }
    }
}

/// `createRendererClipboardAdapter` (clipboard.ts:308).
pub fn create_renderer_clipboard_adapter(
    boundary: Box<dyn RendererClipboardBoundary>,
) -> RendererClipboardAdapter {
    RendererClipboardAdapter { boundary }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    // -- test doubles (clipboard-service.test.ts:10-56, host-clipboard.test.ts:11-43)

    #[derive(Default)]
    struct BackendLog {
        events: Vec<String>,
        /// preferredTypes each backend read received (normalization pin)
        read_types: Vec<Vec<String>>,
        dispose_count: u32,
    }

    /// The recording backend (defaults: read empty / write written /
    /// clear cleared — clipboard-service.test.ts:23-36).
    struct FakeBackend {
        log: Rc<RefCell<BackendLog>>,
        host_status: HostClipboardWriteResult,
        /// aborts the operation signal during writeText, then reports
        /// cancelled (the sync stand-in for the mid-flight abort paths,
        /// clipboard-service.test.ts:132-151)
        abort_on_write: bool,
    }

    impl FakeBackend {
        fn new(log: Rc<RefCell<BackendLog>>) -> Self {
            FakeBackend {
                log,
                host_status: HostClipboardWriteResult::Written,
                abort_on_write: false,
            }
        }
    }

    impl HostClipboardBackend for FakeBackend {
        fn read(&mut self, options: &HostClipboardReadOptions) -> ClipboardReadResult {
            let mut log = self.log.borrow_mut();
            log.events.push("host-read".into());
            log.read_types.push(options.preferred_types.clone());
            ClipboardReadResult::Empty
        }
        fn write_text(
            &mut self,
            _text: &str,
            options: &HostClipboardWriteOptions,
        ) -> HostClipboardWriteResult {
            self.log.borrow_mut().events.push("host-write".into());
            if self.abort_on_write {
                options.signal.abort();
                return HostClipboardWriteResult::Cancelled;
            }
            self.host_status.clone()
        }
        fn clear(&mut self, _options: &HostClipboardWriteOptions) -> HostClipboardClearResult {
            self.log.borrow_mut().events.push("host-clear".into());
            HostClipboardClearResult::Cleared
        }
        fn dispose(&mut self) {
            self.log.borrow_mut().dispose_count += 1;
        }
    }

    /// The recording terminal adapter (clipboard-service.test.ts:39-50).
    struct FakeTerminal {
        log: Rc<RefCell<BackendLog>>,
        remote: bool,
    }

    impl TerminalClipboardAdapter for FakeTerminal {
        fn remote(&self) -> bool {
            self.remote
        }
        fn write_text(
            &mut self,
            _text: &str,
            _selection: ClipboardSelection,
        ) -> TerminalClipboardOperationResult {
            self.log.borrow_mut().events.push("terminal-write".into());
            TerminalClipboardOperationResult {
                attempted: Some(true),
                capability: Osc52Support::Supported,
            }
        }
        fn clear(&mut self, _selection: ClipboardSelection) -> TerminalClipboardOperationResult {
            self.log.borrow_mut().events.push("terminal-clear".into());
            TerminalClipboardOperationResult {
                attempted: Some(true),
                capability: Osc52Support::Supported,
            }
        }
    }

    struct Services {
        clipboard: ClipboardService,
        log: Rc<RefCell<BackendLog>>,
    }

    /// `createServices` (clipboard-service.test.ts:13-56).
    fn create_services(host_status: HostClipboardWriteResult, remote: bool) -> Services {
        let log = Rc::new(RefCell::new(BackendLog::default()));
        let backend = FakeBackend {
            log: Rc::clone(&log),
            host_status,
            abort_on_write: false,
        };
        let host =
            create_host_clipboard_with_backend(HostClipboardOptions::default(), Box::new(backend))
                .expect("host");
        let terminal = FakeTerminal {
            log: Rc::clone(&log),
            remote,
        };
        Services {
            clipboard: create_clipboard(ClipboardOptions {
                host,
                terminal: Box::new(terminal),
            }),
            log,
        }
    }

    fn write_opts(
        destination: ClipboardWriteDestination,
        allow_remote_host: bool,
        signal: Option<AbortSignal>,
    ) -> ClipboardWriteOptions {
        ClipboardWriteOptions {
            destination: Some(destination),
            selection: None,
            allow_remote_host,
            signal,
        }
    }

    // -- createClipboard (clipboard-service.test.ts:58-205) --------------------

    /// "applies local destination policies and best-available
    /// stop/fallback" (clipboard-service.test.ts:59-80).
    #[test]
    fn destination_policies_and_best_available_fallback() {
        // [destination, hostStatus, expectedHost, expectedTerminal, events]
        let cases: &[(
            ClipboardWriteDestination,
            HostClipboardWriteResult,
            &str,
            &str,
            &[&str],
        )] = &[
            (
                ClipboardWriteDestination::TerminalOnly,
                HostClipboardWriteResult::Written,
                "not-attempted",
                "attempted",
                &["terminal-write"],
            ),
            (
                ClipboardWriteDestination::HostOnly,
                HostClipboardWriteResult::Written,
                "written",
                "not-attempted",
                &["host-write"],
            ),
            (
                ClipboardWriteDestination::BestAvailable,
                HostClipboardWriteResult::Written,
                "written",
                "not-attempted",
                &["host-write"],
            ),
            (
                ClipboardWriteDestination::BestAvailable,
                HostClipboardWriteResult::Cancelled,
                "cancelled",
                "not-attempted",
                &["host-write"],
            ),
            (
                ClipboardWriteDestination::BestAvailable,
                HostClipboardWriteResult::Unsupported,
                "unsupported",
                "attempted",
                &["host-write", "terminal-write"],
            ),
            (
                ClipboardWriteDestination::AllAvailable,
                HostClipboardWriteResult::Written,
                "written",
                "attempted",
                &["host-write", "terminal-write"],
            ),
        ];
        for &(destination, ref host_status, expected_host, expected_terminal, events) in cases {
            let mut service = create_services(host_status.clone(), false);
            let result = service
                .clipboard
                .write_text("text", &write_opts(destination, false, None))
                .expect("write");
            assert_eq!(
                (result.host.status_str(), result.terminal.status_str()),
                (expected_host, expected_terminal),
                "destination {destination:?}"
            );
            assert_eq!(
                service.log.borrow().events,
                events,
                "destination {destination:?}"
            );
            service.clipboard.dispose();
        }

        let mut clear_service = create_services(HostClipboardWriteResult::Written, false);
        clear_service
            .clipboard
            .clear(&write_opts(
                ClipboardWriteDestination::AllAvailable,
                false,
                None,
            ))
            .expect("clear");
        assert_eq!(
            clear_service.log.borrow().events,
            vec!["host-clear".to_string(), "terminal-clear".to_string()]
        );
        clear_service.clipboard.dispose();
    }

    /// "enforces remote host authorization for every policy"
    /// (clipboard-service.test.ts:82-96).
    #[test]
    fn remote_host_authorization() {
        let cases: &[(ClipboardWriteDestination, bool, &str, &str, &[&str])] = &[
            (
                ClipboardWriteDestination::HostOnly,
                false,
                "not-attempted",
                "not-attempted",
                &[],
            ),
            (
                ClipboardWriteDestination::HostOnly,
                true,
                "written",
                "not-attempted",
                &["host-write"],
            ),
            (
                ClipboardWriteDestination::BestAvailable,
                true,
                "not-attempted",
                "attempted",
                &["terminal-write"],
            ),
            (
                ClipboardWriteDestination::AllAvailable,
                false,
                "not-attempted",
                "attempted",
                &["terminal-write"],
            ),
        ];
        for &(destination, allow_remote_host, expected_host, expected_terminal, events) in cases {
            let mut service = create_services(HostClipboardWriteResult::Written, true);
            let result = service
                .clipboard
                .write_text("text", &write_opts(destination, allow_remote_host, None))
                .expect("write");
            assert_eq!(
                (result.host.status_str(), result.terminal.status_str()),
                (expected_host, expected_terminal),
                "destination {destination:?} allowRemoteHost {allow_remote_host}"
            );
            assert_eq!(
                service.log.borrow().events,
                events,
                "destination {destination:?} allowRemoteHost {allow_remote_host}"
            );
            service.clipboard.dispose();
        }
    }

    /// "rejects invalid text before either destination"
    /// (clipboard-service.test.ts:98-103).
    #[test]
    fn rejects_invalid_text_before_either_destination() {
        let log = Rc::new(RefCell::new(BackendLog::default()));
        let host = create_host_clipboard_with_backend(
            HostClipboardOptions {
                max_write_bytes: Some(4.0),
                ..Default::default()
            },
            Box::new(FakeBackend::new(Rc::clone(&log))),
        )
        .unwrap();
        let mut clipboard = create_clipboard(ClipboardOptions {
            host,
            terminal: Box::new(FakeTerminal {
                log: Rc::clone(&log),
                remote: false,
            }),
        });

        let err = clipboard
            .write_text(
                "bad\0text",
                &write_opts(ClipboardWriteDestination::AllAvailable, false, None),
            )
            .unwrap_err();
        assert_eq!(err.kind, ClipboardErrorKind::Type);
        assert!(err.message.contains("NUL"));
        assert!(log.borrow().events.is_empty());
        clipboard.dispose();
    }

    /// "validates before handling pre-abort and does not dispatch
    /// pre-aborted operations" (clipboard-service.test.ts:105-130).
    #[test]
    fn validates_before_pre_abort_and_skips_dispatch() {
        let log = Rc::new(RefCell::new(BackendLog::default()));
        let host = create_host_clipboard_with_backend(
            HostClipboardOptions {
                max_write_bytes: Some(4.0),
                ..Default::default()
            },
            Box::new(FakeBackend::new(Rc::clone(&log))),
        )
        .unwrap();
        let mut clipboard = create_clipboard(ClipboardOptions {
            host,
            terminal: Box::new(FakeTerminal {
                log: Rc::clone(&log),
                remote: false,
            }),
        });

        let pre_aborted = AbortSignal::new();
        pre_aborted.abort();
        let options = write_opts(
            ClipboardWriteDestination::AllAvailable,
            false,
            Some(pre_aborted.clone()),
        );

        // validation order: text → destination → selection → preferredTypes
        let empty = clipboard.write_text("", &options).unwrap_err();
        assert!(empty.message.contains("non-empty"), "{}", empty.message);
        let bad_destination = clipboard
            .write_text(
                "text",
                &ClipboardWriteOptions {
                    destination: None,
                    ..options.clone()
                },
            )
            .unwrap_err();
        assert!(bad_destination.message.contains("destination"));
        // the port's enum makes an invalid selection unrepresentable;
        // the string parser carries the TypeError path (test :117)
        assert_eq!(
            ClipboardSelection::parse(Some("invalid"))
                .unwrap_err()
                .message,
            "selection must be clipboard or primary"
        );
        let no_types = clipboard
            .read(&ClipboardReadOptions {
                preferred_types: vec![],
                signal: Some(pre_aborted.clone()),
                ..Default::default()
            })
            .unwrap_err();
        assert!(no_types.message.contains("at least one"));

        // pre-aborted with valid inputs: not-attempted everywhere, no events
        let write = clipboard.write_text("text", &options).unwrap();
        let clear = clipboard.clear(&options).unwrap();
        assert_eq!(write.host.status_str(), "not-attempted");
        assert_eq!(write.terminal.status_str(), "not-attempted");
        assert_eq!(write.terminal.capability, Osc52Support::Unknown);
        assert_eq!(clear.host.status_str(), "not-attempted");
        assert_eq!(clear.terminal.status_str(), "not-attempted");
        assert!(log.borrow().events.is_empty());
        clipboard.dispose();
    }

    /// "preserves terminal dispatch when all-available host work is
    /// later cancelled" (clipboard-service.test.ts:132-151) — the sync
    /// stand-in: the backend aborts the operation signal mid-call and
    /// resolves cancelled; all-available dispatches the terminal
    /// unconditionally (clipboard.ts:224).
    #[test]
    fn all_available_keeps_terminal_when_host_cancels() {
        let log = Rc::new(RefCell::new(BackendLog::default()));
        let host = create_host_clipboard_with_backend(
            HostClipboardOptions::default(),
            Box::new(FakeBackend {
                log: Rc::clone(&log),
                host_status: HostClipboardWriteResult::Written,
                abort_on_write: true,
            }),
        )
        .unwrap();
        let mut clipboard = create_clipboard(ClipboardOptions {
            host,
            terminal: Box::new(FakeTerminal {
                log: Rc::clone(&log),
                remote: false,
            }),
        });

        let result = clipboard
            .write_text(
                "text",
                &write_opts(ClipboardWriteDestination::AllAvailable, false, None),
            )
            .unwrap();
        assert_eq!(
            (result.host.status_str(), result.terminal.status_str()),
            ("cancelled", "attempted")
        );
        assert_eq!(
            log.borrow().events,
            vec!["host-write".to_string(), "terminal-write".to_string()]
        );
        clipboard.dispose();
    }

    /// "does not start a best-available terminal fallback after
    /// cancellation" (clipboard-service.test.ts:153-167) — the sync
    /// stand-in: the host reports unsupported AND aborts the signal
    /// during its call; the `!signal.aborted` gate
    /// (clipboard.ts:216) suppresses the fallback.
    #[test]
    fn best_available_fallback_gated_by_abort() {
        let log = Rc::new(RefCell::new(BackendLog::default()));
        let host = create_host_clipboard_with_backend(
            HostClipboardOptions::default(),
            Box::new(FakeBackend {
                log: Rc::clone(&log),
                host_status: HostClipboardWriteResult::Unsupported,
                abort_on_write: true,
            }),
        )
        .unwrap();
        let mut clipboard = create_clipboard(ClipboardOptions {
            host,
            terminal: Box::new(FakeTerminal {
                log: Rc::clone(&log),
                remote: false,
            }),
        });

        let result = clipboard
            .write_text(
                "text",
                &write_opts(ClipboardWriteDestination::BestAvailable, false, None),
            )
            .unwrap();
        assert_eq!(result.terminal.status_str(), "not-attempted");
        assert_eq!(result.terminal.capability, Osc52Support::Unknown);
        assert_eq!(log.borrow().events, vec!["host-write".to_string()]);
        clipboard.dispose();
    }

    /// "owns host disposal, waits for active composition, and rejects
    /// later operations" (clipboard-service.test.ts:169-204) — the
    /// sync reductions: dispose is idempotent, disposes the backend
    /// exactly once, and later operations reject with "disposed".
    #[test]
    fn dispose_is_idempotent_and_rejects_later_operations() {
        let log = Rc::new(RefCell::new(BackendLog::default()));
        let host = create_host_clipboard_with_backend(
            HostClipboardOptions::default(),
            Box::new(FakeBackend::new(Rc::clone(&log))),
        )
        .unwrap();
        let mut clipboard = create_clipboard(ClipboardOptions {
            host,
            terminal: Box::new(FakeTerminal {
                log: Rc::clone(&log),
                remote: false,
            }),
        });

        clipboard.dispose();
        clipboard.dispose();
        assert_eq!(log.borrow().dispose_count, 1);

        assert!(clipboard
            .read(&ClipboardReadOptions {
                preferred_types: vec!["text/plain".into()],
                ..Default::default()
            })
            .unwrap_err()
            .message
            .contains("disposed"));
        assert!(clipboard
            .write_text(
                "text",
                &write_opts(ClipboardWriteDestination::HostOnly, false, None)
            )
            .unwrap_err()
            .message
            .contains("disposed"));
        assert!(clipboard
            .clear(&write_opts(
                ClipboardWriteDestination::HostOnly,
                false,
                None
            ))
            .unwrap_err()
            .message
            .contains("disposed"));
    }

    // -- createRendererClipboardAdapter (clipboard-service.test.ts:207-236) ---

    struct FakeBoundary {
        capabilities: Rc<RefCell<Option<BoundaryCapabilities>>>,
        calls: Rc<RefCell<Vec<(&'static str, u32)>>>,
    }

    impl RendererClipboardBoundary for FakeBoundary {
        fn capabilities(&self) -> Option<BoundaryCapabilities> {
            *self.capabilities.borrow()
        }
        fn copy_to_clipboard_osc52(
            &mut self,
            _text: &str,
            target: Option<ClipboardTarget>,
        ) -> bool {
            self.calls
                .borrow_mut()
                .push(("write", target.unwrap().as_u32()));
            true
        }
        fn clear_clipboard_osc52(&mut self, target: Option<ClipboardTarget>) -> bool {
            self.calls
                .borrow_mut()
                .push(("clear", target.unwrap().as_u32()));
            false
        }
    }

    /// "maps selections, capabilities, results, and conservative remote
    /// state" (clipboard-service.test.ts:208-236) — the boundary's
    /// capabilities are mutated between calls exactly like upstream
    /// mutates `renderer.capabilities` (:224, :227).
    #[test]
    fn renderer_adapter_maps_selections_capabilities_results_remote() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let capabilities = Rc::new(RefCell::new(None));
        let mut adapter = create_renderer_clipboard_adapter(Box::new(FakeBoundary {
            capabilities: Rc::clone(&capabilities),
            calls: Rc::clone(&calls),
        }));

        // capabilities null → remote defaults TRUE (clipboard.ts:316)
        assert!(adapter.remote());
        let write = adapter.write_text("text", ClipboardSelection::Primary);
        assert_eq!(
            (write.status_str(), write.capability.as_str()),
            ("attempted", "unknown")
        );

        // { remote: false, osc52_support: "supported" } (test :224-226)
        *capabilities.borrow_mut() = Some(BoundaryCapabilities {
            remote: false,
            osc52_support: Osc52Support::Supported,
        });
        assert!(!adapter.remote());
        let clear = adapter.clear(ClipboardSelection::Clipboard);
        assert_eq!(
            (clear.status_str(), clear.capability.as_str()),
            ("local-failure", "supported")
        );

        // osc52 unsupported → not-attempted, no boundary call (:227-231)
        *capabilities.borrow_mut() = Some(BoundaryCapabilities {
            remote: false,
            osc52_support: Osc52Support::Unsupported,
        });
        let write = adapter.write_text("ignored", ClipboardSelection::Clipboard);
        assert_eq!(
            (write.status_str(), write.capability.as_str()),
            ("not-attempted", "unsupported")
        );

        assert_eq!(*calls.borrow(), vec![("write", 1u32), ("clear", 0u32)]);
    }

    // -- ClipboardTarget ABI (clipboard.test.ts:19-24) -------------------------

    /// "preserves the native selection target ABI".
    #[test]
    fn clipboard_target_abi() {
        assert_eq!(ClipboardTarget::Clipboard.as_u32(), 0);
        assert_eq!(ClipboardTarget::Primary.as_u32(), 1);
        assert_eq!(ClipboardTarget::Select.as_u32(), 2);
        assert_eq!(ClipboardTarget::Secondary.as_u32(), 3);
    }

    // -- host service (host-clipboard.test.ts) --------------------------------

    /// "validates configuration before dispatch"
    /// (host-clipboard.test.ts:49-69).
    #[test]
    fn host_validates_configuration_before_dispatch() {
        let backend = || {
            let log = Rc::new(RefCell::new(BackendLog::default()));
            let b: Box<dyn HostClipboardBackend> = Box::new(FakeBackend::new(Rc::clone(&log)));
            (b, log)
        };

        let invalid_u32 = [-1.0, 1.5, f64::NAN, f64::INFINITY, 4_294_967_296.0];
        for &value in &invalid_u32 {
            let (b, _) = backend();
            let err = create_host_clipboard_with_backend(
                HostClipboardOptions {
                    timeout_ms: Some(value),
                    ..Default::default()
                },
                b,
            )
            .unwrap_err();
            assert_eq!(err.kind, ClipboardErrorKind::Range, "timeoutMs {value}");
        }
        for name in [
            "maxReadBytes",
            "maxWriteBytes",
            "maxImagePixels",
            "maxConversionBytes",
            "maxConcurrentOperations",
            "maxProviderTransfers",
        ] {
            let (b, _) = backend();
            let options = HostClipboardOptions {
                max_read_bytes: None,
                max_write_bytes: None,
                max_image_pixels: None,
                max_conversion_bytes: None,
                max_concurrent_operations: None,
                max_provider_transfers: None,
                ..Default::default()
            };
            // inject the invalid value by name
            let options = set_option_by_name(options, name, -1.0);
            let err = create_host_clipboard_with_backend(options, b).unwrap_err();
            assert_eq!(err.kind, ClipboardErrorKind::Range, "{name}");
        }
        for name in ["maxConcurrentOperations", "maxProviderTransfers"] {
            let (b, _) = backend();
            let options = set_option_by_name(HostClipboardOptions::default(), name, 0.0);
            let err = create_host_clipboard_with_backend(options, b).unwrap_err();
            assert_eq!(err.kind, ClipboardErrorKind::Range, "{name}");
        }
        for seat in ["", "seat\0name"] {
            let (b, _) = backend();
            let err = create_host_clipboard_with_backend(
                HostClipboardOptions {
                    wayland_seat: Some(seat.to_owned()),
                    ..Default::default()
                },
                b,
            )
            .unwrap_err();
            assert_eq!(err.kind, ClipboardErrorKind::Type, "seat {seat:?}");
        }
    }

    fn set_option_by_name(
        mut options: HostClipboardOptions,
        name: &str,
        value: f64,
    ) -> HostClipboardOptions {
        match name {
            "maxReadBytes" => options.max_read_bytes = Some(value),
            "maxWriteBytes" => options.max_write_bytes = Some(value),
            "maxImagePixels" => options.max_image_pixels = Some(value),
            "maxConversionBytes" => options.max_conversion_bytes = Some(value),
            "maxConcurrentOperations" => options.max_concurrent_operations = Some(value),
            "maxProviderTransfers" => options.max_provider_transfers = Some(value),
            _ => {}
        }
        options
    }

    /// "does not dispatch backend work when the operation timeout is
    /// zero" (host-clipboard.test.ts:71-83).
    #[test]
    fn host_timeout_zero_skips_dispatch() {
        let log = Rc::new(RefCell::new(BackendLog::default()));
        let mut host = create_host_clipboard_with_backend(
            HostClipboardOptions {
                timeout_ms: Some(0.0),
                ..Default::default()
            },
            Box::new(FakeBackend::new(Rc::clone(&log))),
        )
        .unwrap();

        let read = host
            .read(&ClipboardReadOptions {
                preferred_types: vec!["text/plain".into()],
                ..Default::default()
            })
            .unwrap();
        let write = host
            .write_text("text", &HostClipboardOperationOptions::default())
            .unwrap();
        let clear = host
            .clear(&HostClipboardOperationOptions::default())
            .unwrap();
        assert_eq!(
            (read.status_str(), write.status_str(), clear.status_str()),
            ("timed-out", "timed-out", "timed-out")
        );
        assert!(log.borrow().events.is_empty());
        host.dispose();
    }

    /// "normalizes defaults, selections, MIME types, signals, and
    /// timeout values" (host-clipboard.test.ts:85-103) — the recorded
    /// backend options carry the normalized shape.
    #[test]
    fn host_normalizes_defaults() {
        #[derive(Default)]
        struct Recorder {
            read: Option<HostClipboardReadOptions>,
            write: Option<(String, HostClipboardWriteOptions)>,
            clear: Option<HostClipboardWriteOptions>,
        }
        struct RecorderBackend(Rc<RefCell<Recorder>>);
        impl HostClipboardBackend for RecorderBackend {
            fn read(&mut self, options: &HostClipboardReadOptions) -> ClipboardReadResult {
                self.0.borrow_mut().read = Some(options.clone());
                ClipboardReadResult::Empty
            }
            fn write_text(
                &mut self,
                text: &str,
                options: &HostClipboardWriteOptions,
            ) -> HostClipboardWriteResult {
                self.0.borrow_mut().write = Some((text.to_owned(), options.clone()));
                HostClipboardWriteResult::Written
            }
            fn clear(&mut self, options: &HostClipboardWriteOptions) -> HostClipboardClearResult {
                self.0.borrow_mut().clear = Some(options.clone());
                HostClipboardClearResult::Cleared
            }
            fn dispose(&mut self) {}
        }

        let recorded = Rc::new(RefCell::new(Recorder::default()));
        let mut host = create_host_clipboard_with_backend(
            HostClipboardOptions::default(),
            Box::new(RecorderBackend(Rc::clone(&recorded))),
        )
        .unwrap();

        host.read(&ClipboardReadOptions {
            preferred_types: vec!["Image/PNG".into(), "Text/Plain".into()],
            ..Default::default()
        })
        .unwrap();
        host.write_text("hello", &HostClipboardOperationOptions::default())
            .unwrap();
        host.clear(&HostClipboardOperationOptions {
            selection: Some(ClipboardSelection::Primary),
            signal: None,
        })
        .unwrap();

        let recorded = recorded.borrow();
        let read = recorded.read.as_ref().unwrap();
        assert_eq!(read.preferred_types, vec!["image/png", "text/plain"]);
        assert_eq!(read.selection, ClipboardSelection::Clipboard);
        assert_eq!(read.max_bytes, 8 * 1024 * 1024);
        assert_eq!(read.timeout_ms, 1_000);
        assert!(!read.signal.aborted());
        let (_, write) = recorded.write.as_ref().unwrap();
        assert_eq!(write.selection, ClipboardSelection::Clipboard);
        assert_eq!(write.timeout_ms, 1_000);
        assert_eq!(
            recorded.clear.as_ref().unwrap().selection,
            ClipboardSelection::Primary
        );
        host.dispose();
    }

    /// "validates and normalizes MIME preferences at native protocol
    /// boundaries" (host-clipboard.test.ts:127-154).
    #[test]
    fn host_validates_mime_preferences() {
        let log = Rc::new(RefCell::new(BackendLog::default()));
        let mut host = create_host_clipboard_with_backend(
            HostClipboardOptions::default(),
            Box::new(FakeBackend::new(Rc::clone(&log))),
        )
        .unwrap();

        // [] → TypeError "at least one"
        let err = host
            .read(&ClipboardReadOptions {
                preferred_types: vec![],
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(err.kind, ClipboardErrorKind::Type);
        // parameters → TypeError
        let err = host
            .read(&ClipboardReadOptions {
                preferred_types: vec!["text/plain; charset=utf-8".into()],
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(err.kind, ClipboardErrorKind::Type);
        // bare token → TypeError
        let err = host
            .read(&ClipboardReadOptions {
                preferred_types: vec!["text".into()],
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(err.kind, ClipboardErrorKind::Type);
        // 65 entries → RangeError
        let too_many: Vec<String> = (0..65).map(|i| format!("application/x-{i}")).collect();
        let err = host
            .read(&ClipboardReadOptions {
                preferred_types: too_many,
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(err.kind, ClipboardErrorKind::Range);
        // 244-char subtype → RangeError (255 ASCII byte cap)
        let too_long = format!("application/{}", "a".repeat(244));
        let err = host
            .read(&ClipboardReadOptions {
                preferred_types: vec![too_long],
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(err.kind, ClipboardErrorKind::Range);
        assert!(log.borrow().events.is_empty());

        // 64 entries with a 243-char subtype normalize lowercase and pass
        let mut preferred: Vec<String> = (0..64).map(|i| format!("Application/X-{i}")).collect();
        preferred[63] = format!("Application/{}", "A".repeat(243));
        host.read(&ClipboardReadOptions {
            preferred_types: preferred.clone(),
            ..Default::default()
        })
        .unwrap();
        let expected: Vec<String> = preferred.iter().map(|s| s.to_lowercase()).collect();
        assert_eq!(log.borrow().events.len(), 1);
        // the backend received the lowercased list (test :149-150)
        assert_eq!(log.borrow().read_types[0], expected);
        // foo*bar is a valid essence token char → passes (test :151-152)
        host.read(&ClipboardReadOptions {
            preferred_types: vec!["Application/Foo*Bar".into()],
            ..Default::default()
        })
        .unwrap();
        assert_eq!(log.borrow().events.len(), 2);
        assert_eq!(
            log.borrow().read_types[1],
            vec!["application/foo*bar".to_string()]
        );
        host.dispose();
    }

    /// "validates UTF-8 text and the write limit before dispatch"
    /// (host-clipboard.test.ts:156-177).
    #[test]
    fn host_validates_text_and_write_limit() {
        let log = Rc::new(RefCell::new(BackendLog::default()));
        let mut host = create_host_clipboard_with_backend(
            HostClipboardOptions {
                max_write_bytes: Some(4.0),
                ..Default::default()
            },
            Box::new(FakeBackend::new(Rc::clone(&log))),
        )
        .unwrap();

        for (text, message) in [
            ("", "non-empty"),
            ("a\0b", "NUL"),
            ("hello", "byte limit"),
            ("世界", "byte limit"),
            ("ééé", "byte limit"),
        ] {
            let err = host
                .write_text(text, &HostClipboardOperationOptions::default())
                .unwrap_err();
            assert!(err.message.contains(message), "{text:?} → {}", err.message);
        }
        assert!(log.borrow().events.is_empty());

        // exactly-at-limit values dispatch (4 bytes: "four", "éé", "😀")
        host.write_text("four", &HostClipboardOperationOptions::default())
            .unwrap();
        host.write_text("éé", &HostClipboardOperationOptions::default())
            .unwrap();
        host.write_text("😀", &HostClipboardOperationOptions::default())
            .unwrap();
        assert_eq!(log.borrow().events.len(), 3);
        host.dispose();
    }

    /// "does not dispatch a pre-aborted operation" (host-clipboard.test.ts:179-200).
    #[test]
    fn host_pre_aborted_read_returns_cancelled() {
        let log = Rc::new(RefCell::new(BackendLog::default()));
        let mut host = create_host_clipboard_with_backend(
            HostClipboardOptions::default(),
            Box::new(FakeBackend::new(Rc::clone(&log))),
        )
        .unwrap();

        let pre_aborted = AbortSignal::new();
        pre_aborted.abort();
        let result = host
            .read(&ClipboardReadOptions {
                preferred_types: vec!["text/plain".into()],
                signal: Some(pre_aborted),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.status_str(), "cancelled");
        assert!(log.borrow().events.is_empty());
        host.dispose();
    }

    /// the maxReadBytes re-check on the way out (internal :206-208).
    #[test]
    fn host_oversized_representation_becomes_limit_exceeded() {
        struct BigBackend;
        impl HostClipboardBackend for BigBackend {
            fn read(&mut self, _options: &HostClipboardReadOptions) -> ClipboardReadResult {
                ClipboardReadResult::Read(ClipboardRepresentation {
                    mime_type: "text/plain".into(),
                    bytes: vec![0u8; 9],
                })
            }
            fn write_text(
                &mut self,
                _text: &str,
                _options: &HostClipboardWriteOptions,
            ) -> HostClipboardWriteResult {
                HostClipboardWriteResult::Written
            }
            fn clear(&mut self, _options: &HostClipboardWriteOptions) -> HostClipboardClearResult {
                HostClipboardClearResult::Cleared
            }
            fn dispose(&mut self) {}
        }

        let mut host = create_host_clipboard_with_backend(
            HostClipboardOptions {
                max_read_bytes: Some(8.0),
                ..Default::default()
            },
            Box::new(BigBackend),
        )
        .unwrap();
        let result = host
            .read(&ClipboardReadOptions {
                preferred_types: vec!["text/plain".into()],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.status_str(), "limit-exceeded");
    }
}
