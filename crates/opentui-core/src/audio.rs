//! The audio tier-1 surface — port of `packages/core/src/audio.ts:2752`
//! (`Audio`) for the contract opencode v2's TUI consumes
//! (`packages/tui/src/audio.ts`, the only consumer):
//!
//! - [`Audio::create`] (`{ autoStart: false }`; may throw — the consumer
//!   catches and degrades to `null`)
//! - `.on("error", (err, context))` ([`Audio::on_error`])
//! - `.loadSound(bytes)` ([`Audio::load_sound`]) — decode + cache handle
//! - `.isStarted()` / `.start()` (lazy device/backend init)
//! - `.play(sound, options?)` (volume etc.)
//! - `.dispose()`
//!
//! **Backend honesty.** Upstream `Audio` is a 1:1 wrapper over the zig
//! render lib's audio symbols (`zig.ts:2425-2477`), which wrap a
//! miniaudio engine (`packages/native/src/audio.zig`). The engine is
//! created with `noDevice = TRUE` (native `create`, audio.zig:2562) —
//! creation never touches an audio device; the device is opened lazily
//! by `audioStart`. This crate ports both halves of that seam:
//!
//! - the [`AudioLib`] trait is the render-lib surface the tier-1
//!   members call (`createAudioEngine`, `audioStart`, `audioStop`,
//!   `audioLoad`, `audioPlay`, `destroyAudioEngine`);
//! - [`HeadlessAudioLib`] is the default implementation: a faithful
//!   port of the native engine's *state machine* (status codes, 1-based
//!   sound/voice/group ids, the 32-voice cap, volume `[0,4]` / pan
//!   `[-1,1]` clamps, the implicit `default` group, the noDevice
//!   create-then-lazy-start lifecycle) with **real decoding** of the
//!   formats the consumer feeds — MP3 via `minimp3` (the same decoder
//!   upstream's miniaudio uses: `dr_mp3` builds directly on `minimp3`),
//!   RIFF/WAVE PCM + IEEE-float, and FLAC via `dr_flac` from the
//!   vendored miniaudio (`crates/opentui-core/native/opentui_audio.c`
//!   compiles the exact header upstream's `miniaudio_shim.c` builds).
//!
//! **Output sink.** Default builds are headless by design — a cell-grid
//! library should not open audio hardware on `start()` — so the default
//! sink is in-memory: no device is opened, mixed output is observable
//! only through [`HeadlessAudioLib::render_frames`] (the sink driver),
//! and `audioStart` on the null device always succeeds (0). With the
//! `device-audio` cargo feature enabled, the same `HeadlessAudioLib`
//! opens a **real miniaudio playback device** in `audioStart` — the
//! native `start` path (audio.zig:1596-1683): f32, engine channels /
//! sample rate, `StartOptions` period/profile fields, the device
//! callback pulls mixed frames (mirroring `audioCallback`,
//! audio.zig:2447), init/start failure reports `err_device` (−5) with
//! full rollback and **no silent fallback** to the null sink, and the
//! device's channel count feeds back into `output_channels` when in
//! `1..=MA_MAX_CHANNELS` (audio.zig:1660-1664). The engine is shared
//! between the control thread and the device callback through a mutex
//! (the callback `try_lock`s, silencing on contention, exactly like
//! native's lock-miss path).
//!
//! **Narrowed vs upstream (exact enumeration):**
//!
//! 1. WAV decode supports uncompressed RIFF/WAVE (PCM u8/s16/s24/s32
//!    and IEEE f32/f64), the `dr_wav` formats that matter here;
//!    compressed WAV codecs report `err_decode`.
//! 2. `AudioStartOptions` fields are validated for shape only on the
//!    null sink (default builds have no period/buffer configuration to
//!    apply; with `device-audio` they configure the real device). (The
//!    JS getter-throws → `−1` packing edge is a JS-domain artifact; the
//!    `−1` status is reproducible via an injected lib, as upstream's
//!    tests do.)
//! 3. MP3 samples come back through minimp3's i16 output scaled to
//!    f32 (`i16 / 32768.0`); upstream reads minimp3's float output
//!    directly (same decoder, one quantization step apart). FLAC keeps
//!    dr_flac itself, so its f32 output is bit-exact with upstream.
//! 4. Default-build `audioStart` is the null device: the `err_device`
//!    (−5) no-device failure path is exercised with the `device-audio`
//!    feature (or pinned via an injected [`AudioLib`], mirroring how
//!    upstream's own tests replace `lib.audioStart`, audio.test.ts:254).
//!
//! Capture (`openCapture`/`startCapture`/…), icy streaming
//! (`playStream*`), the recorder (`recordToFile`), device selection and
//! stats remain **todo** — not present here, not pretended.
//!
//! **Emitter semantics.** Upstream extends node `events.EventEmitter`:
//! emitting `"error"` with no listener registered *throws* the error
//! (audio.test.ts:952-963 pins this). The port reproduces that: an
//! error emitted with no [`on_error`] listener panics with the exact
//! upstream message. Callers that register a listener (as opencode's
//! consumer does immediately after create) never see it.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use minimp3::{Decoder, Error as Mp3Error, Frame};

// ---------------------------------------------------------------------------
// shared ABI constants — native audio.zig:1211-1219 (Status),
// audio.zig:220-224 (max_voices / defaults), miniaudio.h:4247
// ---------------------------------------------------------------------------

/// `native_audio.Status.ok` (audio.zig:1211).
pub const AUDIO_STATUS_OK: i32 = 0;
/// `native_audio.Status.err_invalid` (audio.zig:1212).
pub const AUDIO_STATUS_ERR_INVALID: i32 = -1;
/// `native_audio.Status.err_no_space` (audio.zig:1213).
pub const AUDIO_STATUS_ERR_NO_SPACE: i32 = -2;
/// `native_audio.Status.err_decode` (audio.zig:1214).
pub const AUDIO_STATUS_ERR_DECODE: i32 = -3;
/// `native_audio.Status.err_not_found` (audio.zig:1215).
pub const AUDIO_STATUS_ERR_NOT_FOUND: i32 = -4;
/// `native_audio.Status.err_device` (audio.zig:1216).
pub const AUDIO_STATUS_ERR_DEVICE: i32 = -5;

/// `native_audio.max_voices` (audio.zig:220): 32 concurrent
/// voices+streams per engine.
pub const AUDIO_MAX_VOICES: usize = 32;
/// `native_audio.default_sample_rate` (audio.zig:221).
pub const AUDIO_DEFAULT_SAMPLE_RATE: u32 = 48_000;
/// `native_audio.default_playback_channels` (audio.zig:222).
pub const AUDIO_DEFAULT_PLAYBACK_CHANNELS: u32 = 2;
/// `MA_MAX_CHANNELS` (vendored miniaudio.h:4247) — createAudioEngine
/// returns null above this (native `create`, audio.zig:2567).
pub const MA_MAX_CHANNELS: u32 = 254;

/// `DEFAULT_AUDIO_SAMPLE_RATE` (audio.ts:401) — the TS-layer fallback
/// for a non-finite/zero `sampleRate` option.
const DEFAULT_AUDIO_SAMPLE_RATE: u32 = 48_000;
/// `MAX_U32` (audio.ts:405).
const MAX_U32: u32 = 0xffff_ffff;

// ---------------------------------------------------------------------------
// option types — audio.ts:24-54, zig-structs.ts:367-389
// ---------------------------------------------------------------------------

/// `AudioSetupOptions` (audio.ts:24-29). `sample_rate` /
/// `playback_channels` keep the JS `number` domain (f64) so the
/// constructor's normalize-then-clamp semantics (audio.ts:2790-2802)
/// are observable exactly.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioSetupOptions {
    /// `autoStart` — default `false` (audio.ts:2808 `?? false`).
    pub auto_start: Option<bool>,
    /// `sampleRate` — null/non-finite → engine default; else
    /// `trunc`, clamped to `[0, MAX_U32]`.
    pub sample_rate: Option<f64>,
    /// `playbackChannels` — `max(0, trunc)`; 0 → engine default (2);
    /// > `MA_MAX_CHANNELS` (254) → engine creation fails.
    pub playback_channels: Option<f64>,
    /// `startOptions` — default options used by `start()` and
    /// auto-start (audio.ts:2840).
    pub start_options: Option<AudioStartOptions>,
}

/// `AudioStartOptions` (audio.ts:31-47) — device configuration hints.
/// The null device accepts them (they validated for shape upstream
/// packing too) but applies none (see module doc, narrowing #4).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioStartOptions {
    pub period_size_in_frames: Option<f64>,
    pub period_size_in_milliseconds: Option<f64>,
    pub periods: Option<f64>,
    pub performance_profile: Option<f64>,
    pub share_mode: Option<f64>,
    pub no_pre_silenced_output_buffer: Option<bool>,
    pub no_clip: Option<bool>,
    pub no_disable_denormals: Option<bool>,
    pub no_fixed_sized_callback: Option<bool>,
    pub wasapi_no_auto_convert_src: Option<bool>,
    pub wasapi_no_default_quality_src: Option<bool>,
    pub alsa_no_mmap: Option<bool>,
    pub alsa_no_auto_format: Option<bool>,
    pub alsa_no_auto_channels: Option<bool>,
    pub alsa_no_auto_resample: Option<bool>,
}

/// `AudioPlayOptions` (audio.ts:49-54).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioPlayOptions {
    pub volume: Option<f64>,
    pub pan: Option<f64>,
    pub loop_: Option<bool>,
    pub group_id: Option<u32>,
}

/// `AudioCreateOptions` (zig-structs.ts:367) — the packed create
/// payload. `0` in a field means "engine default" (native `create`,
/// audio.zig:2562-2563).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioEngineCreateOptions {
    pub sample_rate: Option<u32>,
    pub playback_channels: Option<u32>,
}

/// `AudioVoiceOptions` (zig-structs.ts:379) — the packed play payload.
/// Fully resolved: the struct packer substitutes `volume=1`, `pan=0`,
/// `loop=false`, `groupId=0` for undefined fields
/// (zig-structs.ts:505-510), and native play() applies the same
/// defaults when no options are passed at all (audio.zig:2208-2212).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NativeAudioVoiceOptions {
    pub volume: f32,
    pub pan: f32,
    pub loop_: bool,
    pub group_id: u32,
}

impl NativeAudioVoiceOptions {
    /// The resolution `Audio.play` performs (audio.ts:2955-2962 +
    /// zig-structs.ts:505-510): explicit value, else the pack default.
    /// The f64→f32 cast is the JS-number→`"f32"` pack coercion.
    fn from_play_options(options: &AudioPlayOptions) -> Self {
        Self {
            volume: options.volume.unwrap_or(1.0) as f32,
            pan: options.pan.unwrap_or(0.0) as f32,
            loop_: options.loop_.unwrap_or(false),
            group_id: options.group_id.unwrap_or(0),
        }
    }

    /// Native play() with no options buffer (audio.zig:2208-2212).
    fn defaults() -> Self {
        Self {
            volume: 1.0,
            pan: 0.0,
            loop_: false,
            group_id: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// handles + results — audio.ts:216-218, zig.ts:2425-2477
// ---------------------------------------------------------------------------

/// `AudioSound` (audio.ts:218) — a decoded sound handle. 1-based
/// (native `load`, audio.zig:2110-2117).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioSound(pub u32);

/// `AudioVoice` (audio.ts:217) — a playing voice slot. 1-based
/// (native `play`, audio.zig:2241).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioVoice(pub u32);

/// `AudioGroup` (audio.ts:216) — a sound group id. 0 = the implicit
/// `default` group (native `initDefaultGroup`, audio.zig:1226).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioGroup(pub u32);

/// An engine handle into an [`AudioLib`] (zig.ts `AudioEngineHandle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioEngineHandle(pub u32);

/// `lib.audioLoad` result (zig.ts:2472): `{ status, soundId }` —
/// `soundId` null unless `status === 0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioLoadResult {
    pub status: i32,
    pub sound_id: Option<u32>,
}

/// `lib.audioPlay` result (zig.ts:2478-2490): `{ status, voiceId }` —
/// `voiceId` null unless `status === 0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPlayResult {
    pub status: i32,
    pub voice_id: Option<u32>,
}

// ---------------------------------------------------------------------------
// errors + error contexts — audio.ts:312-369, 393-395
// ---------------------------------------------------------------------------

/// `AudioAction` (audio.ts:312-339) — the action string carried by
/// [`AudioErrorContext`]. All variants are defined (they are one enum
/// upstream); only the ported members ever emit them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioAction {
    CreateAudioEngine,
    Start,
    StartMixer,
    Stop,
    LoadSound,
    LoadSoundFile,
    UnloadSound,
    Group,
    Play,
    StopVoice,
    SetVoiceGroup,
    SetGroupVolume,
    SetMasterVolume,
    MixFrames,
    EnableTap,
    ReadTapFrames,
    ListPlaybackDevices,
    SelectPlaybackDevice,
    ClearPlaybackDeviceSelection,
    ListCaptureDevices,
    SelectCaptureDevice,
    ClearCaptureDeviceSelection,
    StartCapture,
    ReadCaptureFrames,
    GetCaptureStats,
    StopCapture,
    GetStats,
}

impl AudioAction {
    /// The exact upstream action string (used in
    /// `Audio ${action} failed: ${status}`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CreateAudioEngine => "createAudioEngine",
            Self::Start => "start",
            Self::StartMixer => "startMixer",
            Self::Stop => "stop",
            Self::LoadSound => "loadSound",
            Self::LoadSoundFile => "loadSoundFile",
            Self::UnloadSound => "unloadSound",
            Self::Group => "group",
            Self::Play => "play",
            Self::StopVoice => "stopVoice",
            Self::SetVoiceGroup => "setVoiceGroup",
            Self::SetGroupVolume => "setGroupVolume",
            Self::SetMasterVolume => "setMasterVolume",
            Self::MixFrames => "mixFrames",
            Self::EnableTap => "enableTap",
            Self::ReadTapFrames => "readTapFrames",
            Self::ListPlaybackDevices => "listPlaybackDevices",
            Self::SelectPlaybackDevice => "selectPlaybackDevice",
            Self::ClearPlaybackDeviceSelection => "clearPlaybackDeviceSelection",
            Self::ListCaptureDevices => "listCaptureDevices",
            Self::SelectCaptureDevice => "selectCaptureDevice",
            Self::ClearCaptureDeviceSelection => "clearCaptureDeviceSelection",
            Self::StartCapture => "startCapture",
            Self::ReadCaptureFrames => "readCaptureFrames",
            Self::GetCaptureStats => "getCaptureStats",
            Self::StopCapture => "stopCapture",
            Self::GetStats => "getStats",
        }
    }
}

impl fmt::Display for AudioAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `AudioErrorContext` (audio.ts:341-344): `{ action, status? }`.
/// `status` is `undefined` for the "engine unavailable" emissions
/// (audio.ts:2837, 2898, 2966).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioErrorContext {
    pub action: AudioAction,
    pub status: Option<i32>,
}

/// The `Error` object an `"error"` emission carries. Upstream builds
/// `new Error(message)` — either the explicit message or
/// `Audio ${action} failed: ${status}` (`statusToError`,
/// audio.ts:393-395, with `status ?? -1` at audio.ts:2828).
///
/// The optional `cause` (audio.ts:2829) is exercised only by capture
/// paths, which are todo; no ported emission sets one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioError {
    pub message: String,
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AudioError {}

/// `AudioInitializationAction` (audio.ts:356).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioInitializationAction {
    ResolveRenderLib,
    CreateAudioEngine,
    Start,
}

impl AudioInitializationAction {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ResolveRenderLib => "resolveRenderLib",
            Self::CreateAudioEngine => "createAudioEngine",
            Self::Start => "start",
        }
    }
}

/// `AudioInitializationError` (audio.ts:358-369) — thrown
/// synchronously by `Audio.create` (resolve/pack/create/start
/// failures). `cause` chains the underlying failure where upstream
/// does (audio.ts:367).
#[derive(Debug)]
pub struct AudioInitializationError {
    pub action: AudioInitializationAction,
    pub message: String,
    pub status: Option<i32>,
    pub cause: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl AudioInitializationError {
    pub fn new(
        action: AudioInitializationAction,
        message: impl Into<String>,
        status: Option<i32>,
    ) -> Self {
        Self {
            action,
            message: message.into(),
            status,
            cause: None,
        }
    }

    pub fn with_cause(mut self, cause: Box<dyn std::error::Error + Send + Sync>) -> Self {
        self.cause = Some(cause);
        self
    }
}

impl fmt::Display for AudioInitializationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AudioInitializationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause.as_deref().map(|c| {
            let erased: &(dyn std::error::Error + 'static) = c;
            erased
        })
    }
}

// ---------------------------------------------------------------------------
// AudioLib — the RenderLib audio seam (zig.ts:2425-2477, narrowed to
// the symbols the tier-1 members call)
// ---------------------------------------------------------------------------

/// The render-lib audio surface `Audio` calls. Upstream reaches these
/// through FFI (`resolveRenderLib()`); the port injects the
/// implementation, which is what upstream's own tests mutate
/// (audio.test.ts:254 replaces `lib.audioStart`). The default is
/// [`HeadlessAudioLib`].
pub trait AudioLib {
    /// `createAudioEngine` (zig.ts:2425): returns null when the engine
    /// cannot be created (native `create`, audio.zig:2560-2584 —
    /// playback channels 0→default, > `MA_MAX_CHANNELS` → null).
    fn create_audio_engine(
        &mut self,
        options: Option<AudioEngineCreateOptions>,
    ) -> Option<AudioEngineHandle>;

    /// `destroyAudioEngine` (zig.ts:2426). Native void that can
    /// throw; the port surfaces a failure so `dispose` retains the
    /// engine and reports the first cleanup error exactly like
    /// upstream (audio.ts:3662-3677, audio.test.ts:2185).
    fn destroy_audio_engine(&mut self, engine: AudioEngineHandle) -> Result<(), AudioError>;

    /// `audioStart` (zig.ts:2453): 0 on success; non-zero status
    /// surfaces through `start()`'s boolean + `"error"` emission.
    /// Option-packing failures report −1 (zig.ts:6394-6401).
    fn audio_start(
        &mut self,
        engine: AudioEngineHandle,
        options: Option<&AudioStartOptions>,
    ) -> i32;

    /// `audioStop` (zig.ts:2455).
    fn audio_stop(&mut self, engine: AudioEngineHandle) -> i32;

    /// `audioLoad` (zig.ts:2472): decode `data`, returning
    /// `{ status, soundId }`.
    fn audio_load(&mut self, engine: AudioEngineHandle, data: &[u8]) -> AudioLoadResult;

    /// `audioPlay` (zig.ts:2478-2490): start a voice, returning
    /// `{ status, voiceId }`. A non-u32 `groupId` short-circuits to
    /// `−1` before the native call (zig.ts:6494) — unrepresentable in
    /// the typed port.
    fn audio_play(
        &mut self,
        engine: AudioEngineHandle,
        sound: u32,
        options: Option<NativeAudioVoiceOptions>,
    ) -> AudioPlayResult;

    /// Downcast seam so callers/tests can inspect the injected
    /// backend (the headless lib's engine state, render driver).
    /// No default body: a default `self`→`&dyn Any` coercion needs
    /// `Self: Sized`, which would vanish from the object vtable.
    fn as_any(&self) -> &dyn std::any::Any;

    /// Mutable half of [`AudioLib::as_any`].
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

// ---------------------------------------------------------------------------
// HeadlessAudioLib — the default backend (see module doc)
// ---------------------------------------------------------------------------

/// A decoded sound in the engine (native `Sound`, audio.zig:268-272).
#[derive(Debug, Clone)]
struct HeadlessSound {
    loaded: bool,
    samples: Vec<f32>,
    channels: u32,
    sample_rate: u32,
}

/// A voice slot (native `Voice`, audio.zig:305-311). Slot occupancy
/// (Some/None) is `active` — the native boolean is redundant here.
#[derive(Debug, Clone)]
struct HeadlessVoice {
    sound_index: usize,
    cursor_frame: usize,
    volume: f32,
    pan: f32,
    loop_: bool,
    group_id: u32,
}

/// A sound group (native `SoundGroup`, audio.zig:330-334). The list's
/// *length* is what the ported surface observes (play's group-id
/// validation, audio.zig:2211); the name is native-parity bookkeeping
/// for the tier-2 `group()` lane.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct HeadlessGroup {
    name: String,
}

/// The headless engine (native `Engine`, audio.zig:340-371): created
/// device-less (`noDevice = TRUE`), started lazily. `sample_rate` /
/// `output_channels` are the create-time config (native
/// `ma_engine_get_sample_rate` reads the same state) — native-parity
/// bookkeeping until the tier-2 stats/device lanes land. Shared
/// between the control thread and (with `device-audio`) the miniaudio
/// device callback via [`Mutex`] — the callback `try_lock`s like
/// native's lock-miss path (audio.zig:2466-2470), so addresses are
/// stable behind an `Arc`.
#[derive(Debug)]
#[allow(dead_code)]
struct HeadlessEngine {
    sample_rate: u32,
    output_channels: u32,
    groups: Vec<HeadlessGroup>,
    sounds: Vec<HeadlessSound>,
    voices: Vec<Option<HeadlessVoice>>,
    started: bool,
    has_device: bool,
    destroyed: bool,
    /// The open playback device (native `Engine.device`) — only with
    /// `device-audio`; `None` otherwise (and always on the null sink).
    #[cfg(feature = "device-audio")]
    device: Option<DeviceHandle>,
}

impl HeadlessEngine {
    fn new(sample_rate: u32, output_channels: u32) -> Self {
        // native initDefaultGroup (audio.zig:1226-1245): every engine
        // owns group 0 "default" before any call.
        Self {
            sample_rate,
            output_channels,
            groups: vec![HeadlessGroup {
                name: "default".to_string(),
            }],
            sounds: Vec::new(),
            voices: (0..AUDIO_MAX_VOICES).map(|_| None).collect(),
            started: false,
            has_device: false,
            destroyed: false,
            #[cfg(feature = "device-audio")]
            device: None,
        }
    }

    fn active_voice_count(&self) -> usize {
        self.voices.iter().filter(|v| v.is_some()).count()
    }
}

/// The default [`AudioLib`]: native-engine state-machine semantics
/// with real MP3/WAV/FLAC decode and a null output device (default
/// builds) or a real miniaudio playback device (the `device-audio`
/// feature). See the module doc for the exact honesty statement.
#[derive(Debug, Default)]
pub struct HeadlessAudioLib {
    engines: Vec<Option<Arc<Mutex<HeadlessEngine>>>>,
}

impl HeadlessAudioLib {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock_engine(engine: &Mutex<HeadlessEngine>) -> MutexGuard<'_, HeadlessEngine> {
        // poisoning only happens if a mixing panics mid-mutation; the
        // state is still consistent enough to keep serving
        engine
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn engine_arc(&self, handle: AudioEngineHandle) -> Option<Arc<Mutex<HeadlessEngine>>> {
        let index = handle.0.checked_sub(1)? as usize;
        self.engines.get(index)?.clone()
    }

    /// The engine's mutex, borrowed for the lifetime of `self` — the
    /// Arc never leaves the Vec while it is borrowed, so the address
    /// outlives the guard.
    fn engine_mutex(&self, handle: AudioEngineHandle) -> Option<&Mutex<HeadlessEngine>> {
        let index = handle.0.checked_sub(1)? as usize;
        Some(self.engines.get(index)?.as_ref()?.as_ref())
    }

    fn engine_mut(&mut self, handle: AudioEngineHandle) -> Option<MutexGuard<'_, HeadlessEngine>> {
        Some(Self::lock_engine(self.engine_mutex(handle)?))
    }

    fn engine(&self, handle: AudioEngineHandle) -> Option<MutexGuard<'_, HeadlessEngine>> {
        Some(Self::lock_engine(self.engine_mutex(handle)?))
    }

    /// The null-sink driver: mix `frame_count` frames of all active
    /// voices into `channels` interleaved channels, advancing voice
    /// cursors (non-looping voices finish and free their slot —
    /// native `reapFinishedVoices`, audio.zig:1241-1252). On the null
    /// sink this is the observable stand-in for device consumption;
    /// with `device-audio` the same mixer ([`mix_frames_into`]) feeds
    /// the real device callback. Linear up/down-mix and balance-law
    /// pan, not miniaudio's full DSP graph. Never called by [`Audio`]
    /// — the tier-1 surface has no mixFrames.
    pub fn render_frames(
        &mut self,
        engine: AudioEngineHandle,
        frame_count: usize,
        channels: usize,
    ) -> Option<Vec<f32>> {
        let mut guard = self.engine_mut(engine)?;
        let mut out = vec![0.0f32; frame_count * channels];
        mix_frames_into(&mut guard, &mut out, channels);
        Some(out)
    }

    /// Test/inspection accessor: how many sounds are loaded
    /// (`getStats().soundsLoaded` upstream — the Audio accessor is
    /// tier-2 and unported).
    #[must_use]
    pub fn sounds_loaded(&self, engine: AudioEngineHandle) -> Option<usize> {
        Some(
            self.engine(engine)?
                .sounds
                .iter()
                .filter(|s| s.loaded)
                .count(),
        )
    }

    /// Test/inspection accessor: active voice count
    /// (`getStats().voicesActive` upstream).
    #[must_use]
    pub fn voices_active(&self, engine: AudioEngineHandle) -> Option<usize> {
        Some(self.engine(engine)?.active_voice_count())
    }

    /// Test/inspection accessor: a loaded sound's decoded shape.
    #[must_use]
    pub fn sound_shape(&self, engine: AudioEngineHandle, sound: u32) -> Option<(usize, u32, u32)> {
        let engine = self.engine(engine)?;
        let sound = engine.sounds.get(sound.checked_sub(1)? as usize)?;
        if !sound.loaded {
            return None;
        }
        Some((sound.samples.len(), sound.channels, sound.sample_rate))
    }

    /// Test/inspection accessor: a voice slot's resolved options.
    #[must_use]
    pub fn voice_options(
        &self,
        engine: AudioEngineHandle,
        voice: u32,
    ) -> Option<NativeAudioVoiceOptions> {
        let engine = self.engine(engine)?;
        let slot = engine
            .voices
            .get(voice.checked_sub(1)? as usize)?
            .as_ref()?;
        Some(NativeAudioVoiceOptions {
            volume: slot.volume,
            pan: slot.pan,
            loop_: slot.loop_,
            group_id: slot.group_id,
        })
    }
}

fn stereo_gains(pan: f32) -> (f32, f32) {
    // balance law (miniaudio's default pan semantics): center = both
    // channels at full gain, a side attenuates toward the other; pan
    // clamped [-1, 1] (already clamped at voice-store time)
    let pan = pan.clamp(-1.0, 1.0);
    (1.0 - pan.max(0.0), 1.0 - (-pan).max(0.0))
}

/// The mixer both sinks share: mix `out.len() / channels` frames of
/// all active voices into `out` (interleaved), advancing voice
/// cursors and reaping finished voices (native
/// `readEngineStereo`-equivalent — the null-sink law: linear
/// up/down-mix, extra channels copy the right channel).
fn mix_frames_into(engine: &mut HeadlessEngine, out: &mut [f32], channels: usize) {
    debug_assert!(channels > 0 && out.len().is_multiple_of(channels));
    let frame_count = out.len() / channels;
    let mut voices = std::mem::take(&mut engine.voices);
    for slot in voices.iter_mut() {
        let Some(voice) = slot.as_mut() else { continue };
        let sound = &engine.sounds[voice.sound_index];
        if !sound.loaded {
            continue;
        }
        let sound_channels = sound.channels as usize;
        let total_frames = sound.samples.len().checked_div(sound_channels).unwrap_or(0);
        let (gl, gr) = stereo_gains(voice.pan);
        for frame in 0..frame_count {
            if voice.cursor_frame >= total_frames {
                if voice.loop_ && total_frames > 0 {
                    voice.cursor_frame = 0;
                } else {
                    // finished — reap the slot
                    *slot = None;
                    break;
                }
            }
            let base = voice.cursor_frame * sound_channels;
            let (l, r) = match sound_channels {
                1 => (sound.samples[base], sound.samples[base]),
                _ => (sound.samples[base], sound.samples[base + 1]),
            };
            let l = l * voice.volume * gl;
            let r = r * voice.volume * gr;
            let dest = frame * channels;
            out[dest] += l;
            if channels > 1 {
                out[dest + 1] += r;
                // extra channels copy the right channel (a
                // deterministic choice — the null sink's law)
                for ch in 2..channels {
                    out[dest + ch] += r;
                }
            }
            voice.cursor_frame += 1;
        }
    }
    engine.voices = voices;
}

// ---------------------------------------------------------------------------
// the real playback device — `device-audio` feature (native `start`,
// audio.zig:1596-1683; FFI in audio_native / native/opentui_audio.c)
// ---------------------------------------------------------------------------

#[cfg(feature = "device-audio")]
mod device_backend {
    use super::{mix_frames_into, HeadlessEngine};
    use std::ffi::c_void;
    use std::fmt;

    /// An owned playback device handle. `Drop` runs the native `stop`
    /// teardown (ma_device_stop + uninit + context uninit,
    /// audio.zig:1692-1704 / native/opentui_audio.c) — so dropping the
    /// engine (or the whole lib) can never leak a running device.
    pub(super) struct DeviceHandle {
        pub(super) raw: *mut crate::audio_native::OpaqueAudioDevice,
    }

    // SAFETY: the handle is moved/destroyed only by the thread that
    // owns the engine (control thread); the audio callback never
    // touches it — it reaches the engine through the separate user
    // pointer, guarded by the engine mutex.
    unsafe impl Send for DeviceHandle {}

    impl fmt::Debug for DeviceHandle {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("DeviceHandle")
                .field("open", &!self.raw.is_null())
                .finish()
        }
    }

    impl Drop for DeviceHandle {
        fn drop(&mut self) {
            if !self.raw.is_null() {
                // SAFETY: raw was created by opentui_audio_device_start
                // and destroyed exactly once (the engine mutex
                // serializes take + drop).
                unsafe { crate::audio_native::opentui_audio_device_destroy(self.raw) };
            }
        }
    }

    /// The JS-number → native-StartOptions u32/u8 packing
    /// (zig-structs.ts:488-503: `ToUint32` semantics, then the u8
    /// truncation native `start` applies, audio.zig:1598).
    fn pack_u32(value: Option<f64>) -> u32 {
        value
            .map(|v| v.trunc().rem_euclid(f64::from(u32::MAX) + 1.0) as u32)
            .unwrap_or(0)
    }

    /// `performance_profile` / `share_mode` pack to u8
    /// (zig-structs.ts "u8" fields).
    fn pack_u8(value: Option<f64>) -> u8 {
        pack_u32(value) as u8
    }

    fn pack_bool(value: Option<bool>) -> i32 {
        i32::from(value.unwrap_or(false))
    }

    /// Native `start`'s device half (audio.zig:1605-1682): configure
    /// f32 / engine channels / engine sample rate + StartOptions,
    /// open + start the device. `Ok((raw, channels))` on success (the
    /// device's actual channel count — audio.zig:1660); `Err(())` is
    /// the `err_device` (−5) case: init/start failure, full rollback
    /// inside the shim.
    pub(super) fn open_device(
        sample_rate: u32,
        output_channels: u32,
        user: *mut c_void,
        options: Option<&super::AudioStartOptions>,
    ) -> Result<(*mut crate::audio_native::OpaqueAudioDevice, u32), ()> {
        use crate::audio_native::{
            opentui_audio_device_start, DeviceDataCallback, OpaqueAudioDevice,
        };
        let default = super::AudioStartOptions::default();
        let options = options.unwrap_or(&default);
        let mut device: *mut OpaqueAudioDevice = std::ptr::null_mut();
        let mut channels = 0u32;
        // SAFETY: `device`/`channels` are valid out-params; `user`
        // must point at the Arc'd engine mutex (the Arc in
        // HeadlessAudioLib keeps it alive for at least as long as the
        // DeviceHandle stored inside it, which is dropped before the
        // engine).
        let result = unsafe {
            opentui_audio_device_start(
                &mut device,
                &mut channels,
                sample_rate,
                output_channels,
                pack_u32(options.period_size_in_frames),
                pack_u32(options.period_size_in_milliseconds),
                pack_u32(options.periods),
                pack_u8(options.performance_profile),
                pack_u8(options.share_mode),
                pack_bool(options.no_pre_silenced_output_buffer),
                pack_bool(options.no_clip),
                pack_bool(options.no_disable_denormals),
                pack_bool(options.no_fixed_sized_callback),
                pack_bool(options.wasapi_no_auto_convert_src),
                pack_bool(options.wasapi_no_default_quality_src),
                pack_bool(options.alsa_no_mmap),
                pack_bool(options.alsa_no_auto_format),
                pack_bool(options.alsa_no_auto_channels),
                pack_bool(options.alsa_no_auto_resample),
                user,
                device_data_callback as DeviceDataCallback,
            )
        };
        if result != 0 || device.is_null() {
            return Err(());
        }
        debug_assert!(channels > 0);
        Ok((device, channels))
    }

    /// Native `audioCallback` (audio.zig:2447-2508), narrowed to the
    /// shim boundary: null user silences (the shim does it); lock-miss
    /// silences (audio.zig:2466-2470); not-started silences + reaps
    /// (audio.zig:2473-2478); otherwise mix. A panic inside the mixer
    /// is swallowed into silence — a C callback must not unwind.
    unsafe extern "C" fn device_data_callback(
        user: *mut c_void,
        out: *mut f32,
        frame_count: u32,
        channels: u32,
    ) {
        if user.is_null() || out.is_null() || frame_count == 0 || channels == 0 {
            return;
        }
        // SAFETY: user points at a live Arc'd Mutex<HeadlessEngine> —
        // the device is destroyed (which joins the audio thread)
        // before that Arc can drop, so the pointer is valid for the
        // whole callback.
        let mutex = unsafe { &*(user.cast::<std::sync::Mutex<HeadlessEngine>>()) };
        let buffer = unsafe {
            std::slice::from_raw_parts_mut(out, frame_count as usize * channels as usize)
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut guard = match mutex.try_lock() {
                Ok(guard) => guard,
                // poisoned: the state is still consistent enough to
                // keep serving (same policy as lock_engine)
                Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
                // native lock-miss (audio.zig:2466-2470)
                Err(std::sync::TryLockError::WouldBlock) => {
                    buffer.fill(0.0);
                    return;
                }
            };
            if !guard.started {
                // audio.zig:2473-2478: silence while not started
                buffer.fill(0.0);
                return;
            }
            mix_frames_into(&mut guard, buffer, channels as usize);
        }));
        if result.is_err() {
            buffer.fill(0.0);
        }
    }
}

#[cfg(feature = "device-audio")]
use device_backend::DeviceHandle;

fn clamp_native_f32(value: f32, lo: f32, hi: f32) -> f32 {
    // native `clamp` (audio.zig:2236-2238)
    value.clamp(lo, hi)
}

impl AudioLib for HeadlessAudioLib {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    /// Native `create` (audio.zig:2560-2584): channels 0 → default 2,
    /// channels above `MA_MAX_CHANNELS` → null. The engine is created
    /// device-less (`noDevice = TRUE`) — creation never opens a device.
    fn create_audio_engine(
        &mut self,
        options: Option<AudioEngineCreateOptions>,
    ) -> Option<AudioEngineHandle> {
        let options = options.unwrap_or_default();
        let sample_rate = match options.sample_rate {
            Some(0) | None => AUDIO_DEFAULT_SAMPLE_RATE,
            Some(rate) => rate,
        };
        let playback_channels = match options.playback_channels {
            Some(0) | None => AUDIO_DEFAULT_PLAYBACK_CHANNELS,
            Some(channels) => channels,
        };
        if playback_channels == 0 || playback_channels > MA_MAX_CHANNELS {
            return None;
        }
        self.engines
            .push(Some(Arc::new(Mutex::new(HeadlessEngine::new(
                sample_rate,
                playback_channels,
            )))));
        Some(AudioEngineHandle(self.engines.len() as u32))
    }

    /// Native `destroy` (audio.zig:2586-2590) — idempotent via handle
    /// table semantics (lib.zig beginDestroy/finishDestroy)
    fn destroy_audio_engine(&mut self, engine: AudioEngineHandle) -> Result<(), AudioError> {
        #[cfg(feature = "device-audio")]
        {
            // drop a live device before the tombstone (native destroy
            // stops the started device first, audio.zig:2586-2590 /
            // stop 1692-1704)
            let device = self
                .engine_mut(engine)
                .and_then(|mut guard| guard.device.take());
            drop(device);
        }
        let index = engine.0.checked_sub(1).map(|i| i as usize);
        if let Some(engine) = index
            .and_then(|i| self.engines.get_mut(i))
            .and_then(|slot| slot.as_mut())
        {
            let mut guard = HeadlessAudioLib::lock_engine(engine);
            guard.destroyed = true;
            guard.started = false;
            guard.has_device = false;
            guard.sounds.clear();
            guard.voices.clear();
        }
        Ok(())
    }

    /// Native `start` (audio.zig:1596-1712): already started+device →
    /// ok; otherwise initialize the context + open the playback
    /// device. On the null sink (default builds) the device open
    /// always succeeds; with `device-audio` a real miniaudio device
    /// opens — `err_device` (−5) when init/start fails or no device
    /// exists, with full rollback and no silent fallback (audio.zig:
    /// 1653-1657, 1672-1680).
    fn audio_start(
        &mut self,
        engine: AudioEngineHandle,
        options: Option<&AudioStartOptions>,
    ) -> i32 {
        let Some(engine_arc) = self.engine_arc(engine) else {
            return AUDIO_STATUS_ERR_INVALID;
        };
        let already_running = {
            let guard = Self::lock_engine(&engine_arc);
            guard.started && guard.has_device
        };
        if already_running {
            return AUDIO_STATUS_OK;
        }
        #[cfg(feature = "device-audio")]
        {
            // the create-time config the device opens with
            // (audio.zig:1631, 1641)
            let (sample_rate, output_channels) = {
                let guard = Self::lock_engine(&engine_arc);
                (guard.sample_rate, guard.output_channels)
            };
            // flip `started` before the device open so a callback that
            // wins the race mixes instead of silencing (audio.zig:1669),
            // remembering the prior value for the rollback
            let was_started = {
                let mut guard = Self::lock_engine(&engine_arc);
                let was = guard.started;
                guard.started = true;
                was
            };
            // pUserData is the Arc'd mutex itself — the engine the
            // callback mixes from (audio.zig:1651)
            let user = Arc::as_ptr(&engine_arc).cast_mut() as *mut std::ffi::c_void;
            let result = device_backend::open_device(sample_rate, output_channels, user, options);
            let mut guard = Self::lock_engine(&engine_arc);
            match result {
                Ok((raw, device_channels)) => {
                    guard.has_device = true;
                    guard.device = Some(DeviceHandle { raw });
                    if device_channels > 0 && device_channels <= MA_MAX_CHANNELS {
                        // audio.zig:1660-1664: the device's channel
                        // count feeds back into the engine config
                        guard.output_channels = device_channels;
                    }
                    AUDIO_STATUS_OK
                }
                Err(()) => {
                    // audio.zig:1673-1680: start failure rolls the
                    // started flag back and drops the device
                    guard.started = was_started;
                    AUDIO_STATUS_ERR_DEVICE
                }
            }
        }
        #[cfg(not(feature = "device-audio"))]
        {
            let _ = options; // the null device has no configuration
                             // null device: context init + device open always succeed
            let mut guard = Self::lock_engine(&engine_arc);
            guard.started = true;
            guard.has_device = true;
            AUDIO_STATUS_OK
        }
    }

    /// Native `stop` (audio.zig:1692-1704): always ok; stops and
    /// drops the device. Voice slots are left as-is (matching native,
    /// which stops the device only).
    fn audio_stop(&mut self, engine: AudioEngineHandle) -> i32 {
        #[cfg(feature = "device-audio")]
        {
            let device = self
                .engine_mut(engine)
                .and_then(|mut guard| guard.device.take());
            drop(device); // ma_device_stop + uninit (audio.zig:1698-1701)
        }
        let Some(mut guard) = self.engine_mut(engine) else {
            return AUDIO_STATUS_ERR_INVALID;
        };
        guard.started = false;
        guard.has_device = false;
        AUDIO_STATUS_OK
    }

    /// Native `load` (audio.zig:2100-2118): empty payload →
    /// `err_invalid`; decode failure → `err_decode`; success appends
    /// and returns the 1-based id (ids are never reused — `unload`
    /// tombstones the slot, audio.zig:2130-2147).
    fn audio_load(&mut self, engine: AudioEngineHandle, data: &[u8]) -> AudioLoadResult {
        let Some(mut engine) = self.engine_mut(engine) else {
            return AudioLoadResult {
                status: AUDIO_STATUS_ERR_INVALID,
                sound_id: None,
            };
        };
        if data.is_empty() {
            return AudioLoadResult {
                status: AUDIO_STATUS_ERR_INVALID,
                sound_id: None,
            };
        }
        let decoded = match decode_sound(data) {
            Ok(sound) => sound,
            Err(()) => {
                return AudioLoadResult {
                    status: AUDIO_STATUS_ERR_DECODE,
                    sound_id: None,
                }
            }
        };
        engine.sounds.push(HeadlessSound {
            loaded: true,
            samples: decoded.samples,
            channels: decoded.channels,
            sample_rate: decoded.sample_rate,
        });
        AudioLoadResult {
            status: AUDIO_STATUS_OK,
            sound_id: Some(engine.sounds.len() as u32),
        }
    }

    /// Native `play` (audio.zig:2196-2243): id 0/out-of-range or
    /// unloaded → `err_not_found`; unknown group → `err_invalid`;
    /// ≥ 32 active voices → `err_no_space`; success claims a free
    /// 1-based slot with volume clamped `[0,4]`, pan `[-1,1]`
    /// (audio.zig:2234-2238).
    fn audio_play(
        &mut self,
        engine: AudioEngineHandle,
        sound_id: u32,
        options: Option<NativeAudioVoiceOptions>,
    ) -> AudioPlayResult {
        let Some(mut engine) = self.engine_mut(engine) else {
            return AudioPlayResult {
                status: AUDIO_STATUS_ERR_INVALID,
                voice_id: None,
            };
        };
        // reapFinishedVoices first (audio.zig:2199)
        // — the null sink reaps in render_frames; finished voices are
        // already None there.
        if sound_id == 0 || sound_id as usize > engine.sounds.len() {
            return AudioPlayResult {
                status: AUDIO_STATUS_ERR_NOT_FOUND,
                voice_id: None,
            };
        }
        let options = options.unwrap_or_else(NativeAudioVoiceOptions::defaults);
        let group_index = options.group_id as usize;
        if group_index >= engine.groups.len() {
            return AudioPlayResult {
                status: AUDIO_STATUS_ERR_INVALID,
                voice_id: None,
            };
        }
        if engine.active_voice_count() >= AUDIO_MAX_VOICES {
            return AudioPlayResult {
                status: AUDIO_STATUS_ERR_NO_SPACE,
                voice_id: None,
            };
        }
        let free_index = engine.voices.iter().position(|v| v.is_none());
        let Some(free_index) = free_index else {
            return AudioPlayResult {
                status: AUDIO_STATUS_ERR_NO_SPACE,
                voice_id: None,
            };
        };
        let sound_index = (sound_id - 1) as usize;
        if !engine.sounds[sound_index].loaded {
            return AudioPlayResult {
                status: AUDIO_STATUS_ERR_NOT_FOUND,
                voice_id: None,
            };
        }
        engine.voices[free_index] = Some(HeadlessVoice {
            sound_index,
            cursor_frame: 0,
            volume: clamp_native_f32(options.volume, 0.0, 4.0),
            pan: clamp_native_f32(options.pan, -1.0, 1.0),
            loop_: options.loop_,
            group_id: options.group_id,
        });
        AudioPlayResult {
            status: AUDIO_STATUS_OK,
            voice_id: Some(free_index as u32 + 1),
        }
    }
}

// ---------------------------------------------------------------------------
// decode — miniaudio's built-in decoders, narrowed (see module doc)
// ---------------------------------------------------------------------------

struct DecodedSound {
    samples: Vec<f32>,
    channels: u32,
    sample_rate: u32,
}

/// `decodeSoundFromMemory` (native audio.zig:1198-1216): miniaudio
/// sniffs the container (WAV by RIFF/WAVE, FLAC by "fLaC", MP3 by
/// frame sync) and decodes to interleaved f32 at the source's native
/// channel count / sample rate (`ma_decoder_config_init(f32, 0, 0)`).
fn decode_sound(bytes: &[u8]) -> Result<DecodedSound, ()> {
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        return decode_wav(bytes);
    }
    if bytes.len() >= 4 && &bytes[0..4] == b"fLaC" {
        // native decodes FLAC through miniaudio's built-in dr_flac —
        // the same decoder the vendored header gives this port
        return match crate::audio_native::decode_flac(bytes) {
            Some((samples, channels, sample_rate)) => Ok(DecodedSound {
                samples,
                channels,
                sample_rate,
            }),
            None => Err(()),
        };
    }
    decode_mp3(bytes)
}

/// dr_wav's uncompressed slice: RIFF/WAVE with PCM (u8/s16/s24/s32) or
/// IEEE-float (f32/f64) `fmt ` payloads. Compressed WAV codecs fail
/// like an unsupported decoder upstream.
fn decode_wav(bytes: &[u8]) -> Result<DecodedSound, ()> {
    let mut pos = 12usize; // past RIFF<size>WAVE
    let mut format = 0u16;
    let mut channels = 0u16;
    let mut sample_rate = 0u32;
    let mut bits_per_sample = 0u16;
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = LE::u32(bytes, pos + 4) as usize;
        let body = pos + 8;
        if id == b"fmt " {
            if size < 16 || body + 16 > bytes.len() {
                return Err(());
            }
            format = LE::u16(bytes, body);
            channels = LE::u16(bytes, body + 2);
            sample_rate = LE::u32(bytes, body + 4);
            bits_per_sample = LE::u16(bytes, body + 14);
        } else if id == b"data" {
            let end = body.saturating_add(size).min(bytes.len());
            data = Some(&bytes[body..end]);
        }
        // chunks are word-aligned; a corrupt size ends the walk
        let Some(next) = body.checked_add(size).and_then(|p| p.checked_add(size & 1)) else {
            break;
        };
        pos = next;
    }
    let Some(data) = data else { return Err(()) };
    if channels == 0 || channels as u32 > MA_MAX_CHANNELS {
        return Err(());
    }
    let bytes_per_sample = match (format, bits_per_sample) {
        (1, 8) => 1,
        (1, 16) => 2,
        (1, 24) => 3,
        (1, 32) => 4,
        (3, 32) => 4,
        (3, 64) => 8,
        _ => return Err(()), // compressed or exotic WAV — unsupported
    };
    if data.len() % (bytes_per_sample * channels as usize) != 0 {
        return Err(());
    }
    let frame_count = data.len() / (bytes_per_sample * channels as usize);
    let mut samples = Vec::with_capacity(frame_count * channels as usize);
    for i in 0..frame_count * channels as usize {
        let base = i * bytes_per_sample;
        let sample = match (format, bits_per_sample) {
            (1, 8) => (data[base] as f32 - 128.0) / 128.0,
            (1, 16) => LE::i16(data, base) as f32 / 32768.0,
            (1, 24) => {
                let raw = ((data[base] as u32)
                    | ((data[base + 1] as u32) << 8)
                    | ((data[base + 2] as u32) << 16)) as i32;
                let raw = if raw >= 1 << 23 { raw - (1 << 24) } else { raw };
                raw as f32 / 8_388_608.0
            }
            (1, 32) => LE::i32(data, base) as f32 / 2147483648.0,
            (3, 32) => LE::f32(data, base),
            (3, 64) => LE::f64(data, base) as f32,
            _ => unreachable!(),
        };
        samples.push(sample.clamp(-1.0, 1.0));
    }
    Ok(DecodedSound {
        samples,
        channels: channels as u32,
        sample_rate,
    })
}

/// dr_mp3 → minimp3: the same decoder. Interleaved frames concatenate
/// into one buffer (native `decodeSoundKnownLength` /
/// `decodeSoundUnknownLength`, audio.zig:1230-1266).
fn decode_mp3(bytes: &[u8]) -> Result<DecodedSound, ()> {
    let mut decoder = Decoder::new(std::io::Cursor::new(bytes));
    let mut samples: Vec<f32> = Vec::new();
    let mut channels = 0usize;
    let mut sample_rate = 0u32;
    let mut frames = 0usize;
    loop {
        match decoder.next_frame() {
            Ok(Frame {
                data,
                sample_rate: rate,
                channels: ch,
                ..
            }) => {
                if frames == 0 {
                    channels = ch;
                    sample_rate = rate.max(0) as u32;
                } else if ch != channels {
                    // miniaudio's decoder rejects mid-stream format
                    // changes; treat as decode failure
                    return Err(());
                }
                samples.extend(data.iter().map(|&s| f32::from(s) / 32768.0));
                frames += 1;
            }
            Err(Mp3Error::Eof) => break,
            Err(_) => {
                // a skippable/IO error inside pure memory decode is a
                // decode failure (there is no transport to blame)
                if frames == 0 {
                    return Err(());
                }
                break;
            }
        }
    }
    if frames == 0 || channels == 0 {
        return Err(());
    }
    if !samples.len().is_multiple_of(channels) {
        return Err(());
    }
    Ok(DecodedSound {
        samples,
        channels: channels as u32,
        sample_rate,
    })
}

/// Minimal little-endian reader helpers (the WAV byte order).
struct LE;

impl LE {
    fn u16(b: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([b[at], b[at + 1]])
    }
    fn u32(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
    }
    fn i16(b: &[u8], at: usize) -> i16 {
        i16::from_le_bytes([b[at], b[at + 1]])
    }
    fn i32(b: &[u8], at: usize) -> i32 {
        i32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
    }
    fn f32(b: &[u8], at: usize) -> f32 {
        f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
    }
    fn f64(b: &[u8], at: usize) -> f64 {
        f64::from_le_bytes([
            b[at],
            b[at + 1],
            b[at + 2],
            b[at + 3],
            b[at + 4],
            b[at + 5],
            b[at + 6],
            b[at + 7],
        ])
    }
}

// ---------------------------------------------------------------------------
// the event emitter — node events.EventEmitter, the AudioEvents map
// (audio.ts:346-354), narrowed to events the ported surface emits
// ---------------------------------------------------------------------------

/// Which `AudioEvents` entry a listener subscribed to (audio.ts:346).
/// `captureStarted`/`captureStopped` are map members but have no emit
/// site in the ported surface (capture is todo) — omitted until the
/// family lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioEventKind {
    Error,
    Started,
    MixerStarted,
    Stopped,
    Disposed,
}

/// A listener registration (`on` returns this; `off` consumes it).
#[derive(Debug)]
pub struct AudioListenerId {
    kind: AudioEventKind,
    index: usize,
}

type ErrorHandler = Box<dyn FnMut(&AudioError, &AudioErrorContext) + Send>;
type UnitHandler = Box<dyn FnMut() + Send>;

#[derive(Default)]
struct AudioEventHandlers {
    error: Vec<Option<ErrorHandler>>,
    started: Vec<Option<UnitHandler>>,
    mixer_started: Vec<Option<UnitHandler>>,
    stopped: Vec<Option<UnitHandler>>,
    disposed: Vec<Option<UnitHandler>>,
}

// ---------------------------------------------------------------------------
// Audio — audio.ts:2752-3683 (tier-1 members; capture/stream/recorder
// families intentionally absent)
// ---------------------------------------------------------------------------

/// `Audio` (audio.ts:2752). Owns one engine inside an injected
/// [`AudioLib`]; all state flags mirror upstream's privates
/// (audio.ts:2770-2784).
pub struct Audio {
    lib: Box<dyn AudioLib + Send>,
    default_start_options: Option<AudioStartOptions>,
    engine: Option<AudioEngineHandle>,
    groups: HashMap<String, u32>,
    playback_started: bool,
    mixer_started: bool,
    disposing: bool,
    sample_rate: u32,
    handlers: AudioEventHandlers,
}

impl fmt::Debug for Audio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Audio")
            .field("sample_rate", &self.sample_rate)
            .field("engine", &self.engine)
            .field("playback_started", &self.playback_started)
            .field("mixer_started", &self.mixer_started)
            .field("disposing", &self.disposing)
            .finish_non_exhaustive()
    }
}

impl Audio {
    /// `Audio.create(options = {})` (audio.ts:2753-2766): resolve the
    /// render lib (throwing `AudioInitializationError("resolveRenderLib",
    /// …)` on failure — the headless lib never fails to resolve), then
    /// construct. Upstream throws; the port returns `Err`.
    pub fn create(options: &AudioSetupOptions) -> Result<Self, AudioInitializationError> {
        Self::create_with_lib(Box::new(HeadlessAudioLib::new()), options)
    }

    /// The `new Audio(lib, options)` half of `create` (audio.ts:2765,
    /// constructor 2786-2818) with the render lib injected — the seam
    /// upstream's tests mutate (`replaceMethod(lib, "audioStart", …)`,
    /// audio.test.ts:254) and where a real device backend would plug
    /// in.
    pub fn create_with_lib(
        lib: Box<dyn AudioLib + Send>,
        options: &AudioSetupOptions,
    ) -> Result<Self, AudioInitializationError> {
        let mut audio = Self {
            lib,
            default_start_options: options.start_options.clone(),
            engine: None,
            groups: HashMap::new(),
            playback_started: false,
            mixer_started: false,
            disposing: false,
            sample_rate: DEFAULT_AUDIO_SAMPLE_RATE,
            handlers: AudioEventHandlers::default(),
        };
        // audio.ts:2790-2794 — normalize sampleRate
        let normalized_sample_rate = match options.sample_rate {
            None => 0,
            Some(value) if !value.is_finite() => 0,
            Some(value) => (value.trunc().clamp(0.0, f64::from(MAX_U32))) as u32,
        };
        audio.sample_rate = if normalized_sample_rate == 0 {
            DEFAULT_AUDIO_SAMPLE_RATE
        } else {
            normalized_sample_rate
        };
        // audio.ts:2795-2802 — createOptions undefined when neither
        // option is given
        let create_options = if options.sample_rate.is_none() && options.playback_channels.is_none()
        {
            None
        } else {
            Some(AudioEngineCreateOptions {
                sample_rate: options.sample_rate.map(|_| normalized_sample_rate),
                playback_channels: options
                    .playback_channels
                    .map(|channels| channels.trunc().max(0.0) as u32),
            })
        };
        // audio.ts:2803-2806
        let engine = audio.lib.create_audio_engine(create_options);
        let Some(engine) = engine else {
            return Err(AudioInitializationError::new(
                AudioInitializationAction::CreateAudioEngine,
                "Audio createAudioEngine returned null",
                None,
            ));
        };
        audio.engine = Some(engine);
        // audio.ts:2808-2817 — auto-start (default false)
        if options.auto_start.unwrap_or(false) {
            let status = audio
                .lib
                .audio_start(engine, audio.default_start_options.as_ref());
            if status != 0 {
                return Err(audio.throw_after_initialization_cleanup(
                    AudioInitializationError::new(
                        AudioInitializationAction::Start,
                        format!("Audio auto-start failed: {status}"),
                        Some(status),
                    ),
                ));
            }
            audio.playback_started = true;
            audio.mixer_started = true;
        }
        Ok(audio)
    }

    /// `throwAfterInitializationCleanup` (audio.ts:2820-2825): drop
    /// and destroy the engine, then surface the error. A destruction
    /// failure is discarded — the init error replaces it (upstream
    /// cannot observe an exception here either; it throws `error`).
    fn throw_after_initialization_cleanup(
        &mut self,
        error: AudioInitializationError,
    ) -> AudioInitializationError {
        if let Some(engine) = self.engine.take() {
            let _ = self.lib.destroy_audio_engine(engine);
        }
        error
    }

    /// `readonly sampleRate` (audio.ts:2768, normalized 2790-2794).
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The injected backend, if it is an `L` (upstream keeps the lib
    /// private; this is the port's inspection seam for the default
    /// headless engine — see [`HeadlessAudioLib`] accessors).
    #[must_use]
    pub fn lib<L: AudioLib + Send + 'static>(&self) -> Option<&L> {
        self.lib.as_ref().as_any().downcast_ref::<L>()
    }

    /// Mutable [`Audio::lib`].
    pub fn lib_mut<L: AudioLib + Send + 'static>(&mut self) -> Option<&mut L> {
        self.lib.as_mut().as_any_mut().downcast_mut::<L>()
    }

    /// The engine handle inside the injected lib (upstream private;
    /// the port's inspection seam, paired with [`Audio::lib`]).
    /// Returns a null handle after dispose.
    #[must_use]
    pub fn engine_handle(&self) -> AudioEngineHandle {
        self.engine.unwrap_or(AudioEngineHandle(0))
    }

    /// `off`/`removeListener` for an `on_*` registration (node
    /// EventEmitter). Slots are stable: a removed id never resolves
    /// to another listener.
    pub fn off(&mut self, id: AudioListenerId) {
        let index = id.index;
        match id.kind {
            AudioEventKind::Error => {
                if let Some(entry) = self.handlers.error.get_mut(index) {
                    *entry = None;
                }
            }
            AudioEventKind::Started => {
                if let Some(entry) = self.handlers.started.get_mut(index) {
                    *entry = None;
                }
            }
            AudioEventKind::MixerStarted => {
                if let Some(entry) = self.handlers.mixer_started.get_mut(index) {
                    *entry = None;
                }
            }
            AudioEventKind::Stopped => {
                if let Some(entry) = self.handlers.stopped.get_mut(index) {
                    *entry = None;
                }
            }
            AudioEventKind::Disposed => {
                if let Some(entry) = self.handlers.disposed.get_mut(index) {
                    *entry = None;
                }
            }
        }
    }

    /// `emitError` (audio.ts:2827-2831): `new Error(message)` or
    /// `statusToError(action, status ?? -1)` (audio.ts:393-395), then
    /// `emit("error", error, { action, status })`. Node's
    /// EventEmitter throws an emitted error that has no listener —
    /// the port panics with the exact message in that case
    /// (audio.test.ts:952-963 pins the upstream behavior).
    fn emit_error(&mut self, action: AudioAction, status: Option<i32>, message: Option<&str>) {
        let message = match message {
            Some(message) => message.to_string(),
            None => format!("Audio {} failed: {}", action, status.unwrap_or(-1)),
        };
        let error = AudioError { message };
        let context = AudioErrorContext { action, status };
        if !self.handlers.error.iter().any(Option::is_some) {
            panic!("{}", error.message);
        }
        for handler in self.handlers.error.iter_mut().flatten() {
            handler(&error, &context);
        }
    }

    /// `start(options?)` (audio.ts:2833-2850): idempotent-true when
    /// already started; engine missing → error + false; non-zero
    /// native status → error + false; success flips both flags and
    /// emits `"started"` once.
    pub fn start(&mut self, options: Option<&AudioStartOptions>) -> bool {
        if self.playback_started {
            return true;
        }
        let Some(engine) = self.engine else {
            self.emit_error(
                AudioAction::Start,
                None,
                Some("Audio engine unavailable during start"),
            );
            return false;
        };
        let start_options = options.or(self.default_start_options.as_ref());
        let status = self.lib.audio_start(engine, start_options);
        if status != 0 {
            self.emit_error(AudioAction::Start, Some(status), None);
            return false;
        }
        self.playback_started = true;
        self.mixer_started = true;
        for handler in self.handlers.started.iter_mut().flatten() {
            handler();
        }
        true
    }

    /// `stop()` (audio.ts:2869-2885) — included because `dispose`
    /// calls it (audio.ts:3656-3658). Mixer already down → true;
    /// native stop always succeeds here → flags reset + `"stopped"`.
    pub fn stop(&mut self) -> bool {
        if !self.mixer_started {
            return true;
        }
        let Some(engine) = self.engine else {
            self.emit_error(
                AudioAction::Stop,
                None,
                Some("Audio engine unavailable during stop"),
            );
            return false;
        };
        let status = self.lib.audio_stop(engine);
        if status != 0 {
            self.emit_error(AudioAction::Stop, Some(status), None);
            return false;
        }
        self.playback_started = false;
        self.mixer_started = false;
        for handler in self.handlers.stopped.iter_mut().flatten() {
            handler();
        }
        true
    }

    /// `isStarted()` (audio.ts:2887-2889).
    #[must_use]
    pub fn is_started(&self) -> bool {
        self.playback_started
    }

    /// `isMixerStarted()` (audio.ts:2891-2893) — start/stop state
    /// observability (upstream audio.test.ts:338, 356).
    #[must_use]
    pub fn is_mixer_started(&self) -> bool {
        self.mixer_started
    }

    /// `loadSound(data)` (audio.ts:2895-2907). Synchronous upstream
    /// and synchronous here — the Promise the opencode consumer sees
    /// is its own `readFile` chain. Engine missing → error + null;
    /// decode failure (`status`/`soundId`) → error + null; success
    /// returns the 1-based handle. `toBytes` (audio.ts:397-399) is a
    /// no-op over `&[u8]`.
    pub fn load_sound(&mut self, data: &[u8]) -> Option<AudioSound> {
        let Some(engine) = self.engine else {
            self.emit_error(
                AudioAction::LoadSound,
                None,
                Some("Audio engine unavailable during loadSound"),
            );
            return None;
        };
        let result = self.lib.audio_load(engine, data);
        if result.status != 0 || result.sound_id.is_none() {
            self.emit_error(AudioAction::LoadSound, Some(result.status), None);
            return None;
        }
        Some(AudioSound(result.sound_id.unwrap_or(0)))
    }

    /// `play(sound, options?)` (audio.ts:2954-2976): resolve the raw
    /// voice options (audio.ts:2955-2962), engine missing → error +
    /// null, non-zero status / null voice → error + null, else the
    /// voice handle.
    pub fn play(
        &mut self,
        sound: AudioSound,
        options: Option<&AudioPlayOptions>,
    ) -> Option<AudioVoice> {
        let raw_options = options.map(NativeAudioVoiceOptions::from_play_options);
        let Some(engine) = self.engine else {
            self.emit_error(
                AudioAction::Play,
                None,
                Some("Audio engine unavailable during play"),
            );
            return None;
        };
        let result = self.lib.audio_play(engine, sound.0, raw_options);
        if result.status != 0 || result.voice_id.is_none() {
            self.emit_error(AudioAction::Play, Some(result.status), None);
            return None;
        }
        Some(AudioVoice(result.voice_id.unwrap_or(0)))
    }

    /// `dispose()` (audio.ts:3612-3682). With capture/streams unported
    /// the cleanup ladder reduces to: stop the mixer if up (emitting
    /// `"stopped"`), clear groups, destroy the engine, null every
    /// flag, emit `"disposed"`. Upstream rethrows the first cleanup
    /// error (audio.ts:3681) and *retains* the engine when native
    /// destruction throws so cleanup can retry (audio.test.ts:2185) —
    /// the port returns the error and keeps `engine` set in that
    /// case; the headless lib never fails. Re-dispose is a no-op
    /// (audio.ts:3613).
    pub fn dispose(&mut self) -> Result<(), AudioError> {
        if self.engine.is_none() || self.disposing {
            return Ok(());
        }
        self.disposing = true;
        let mut first_error: Option<AudioError> = None;
        // (streams/capture cleanup: those families are todo — the
        // sets are structurally empty here)
        if self.mixer_started {
            // runCleanup(() => void this.stop()) (audio.ts:3657) —
            // stop() reports through "error" + boolean and never
            // throws, so the void-discard means its failure is not a
            // dispose error
            let _ = self.stop();
        }
        self.groups.clear();
        let engine = self.engine;
        let mut engine_destroyed = false;
        // (childCleanupFailed is false — no child streams failed)
        if let Err(error) = self
            .lib
            .destroy_audio_engine(engine.expect("checked above"))
        {
            first_error = Some(error);
        } else {
            engine_destroyed = true;
        }
        if engine_destroyed {
            self.engine = None;
            // capture flags (audio.ts:3670-3675) have no ported state
            for handler in self.handlers.disposed.iter_mut().flatten() {
                handler();
            }
        }
        self.disposing = false;
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    }

    // -- events ----------------------------------------------------------

    /// `.on("error", (error, context) => …)` (audio.ts:347, the
    /// consumer's registration — opencode tui audio.ts:11).
    pub fn on_error(
        &mut self,
        handler: impl FnMut(&AudioError, &AudioErrorContext) + Send + 'static,
    ) -> AudioListenerId {
        self.handlers.error.push(Some(Box::new(handler)));
        AudioListenerId {
            kind: AudioEventKind::Error,
            index: self.handlers.error.len() - 1,
        }
    }

    /// `.on("started", …)` (audio.ts:348) — emitted by `start`
    /// (audio.ts:2848).
    pub fn on_started(&mut self, handler: impl FnMut() + Send + 'static) -> AudioListenerId {
        self.handlers.started.push(Some(Box::new(handler)));
        AudioListenerId {
            kind: AudioEventKind::Started,
            index: self.handlers.started.len() - 1,
        }
    }

    /// `.on("mixerStarted", …)` (audio.ts:349). No ported member
    /// emits it (`startMixer` is tier-2 unported; `start` sets the
    /// flag without emitting it, audio.ts:2846-2847) — the slot
    /// exists for map fidelity.
    pub fn on_mixer_started(&mut self, handler: impl FnMut() + Send + 'static) -> AudioListenerId {
        self.handlers.mixer_started.push(Some(Box::new(handler)));
        AudioListenerId {
            kind: AudioEventKind::MixerStarted,
            index: self.handlers.mixer_started.len() - 1,
        }
    }

    /// `.on("stopped", …)` (audio.ts:352) — emitted by `stop`
    /// (audio.ts:2883), which `dispose` runs (audio.ts:3657).
    pub fn on_stopped(&mut self, handler: impl FnMut() + Send + 'static) -> AudioListenerId {
        self.handlers.stopped.push(Some(Box::new(handler)));
        AudioListenerId {
            kind: AudioEventKind::Stopped,
            index: self.handlers.stopped.len() - 1,
        }
    }

    /// `.on("disposed", …)` (audio.ts:353) — emitted by `dispose`
    /// (audio.ts:3676) after the engine is destroyed.
    pub fn on_disposed(&mut self, handler: impl FnMut() + Send + 'static) -> AudioListenerId {
        self.handlers.disposed.push(Some(Box::new(handler)));
        AudioListenerId {
            kind: AudioEventKind::Disposed,
            index: self.handlers.disposed.len() - 1,
        }
    }
}

/// `setupAudio(options = {})` (audio.ts:3685-3687) — `Audio.create`.
///
/// # Errors
/// Propagates [`AudioInitializationError`] exactly like [`Audio::create`].
pub fn setup_audio(options: &AudioSetupOptions) -> Result<Audio, AudioInitializationError> {
    Audio::create(options)
}
