//! Audio tier-1 tests. Ports of the upstream cases that cover the
//! consumer contract (`packages/core/src/tests/audio.test.ts` — the
//! create/start/loadSound/play/dispose members), plus port-level pins
//! of the headless backend's native-faithful semantics
//! (`packages/native/src/audio.zig`) and the opencode v2 consumer
//! flow (`packages/tui/src/audio.ts`).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::audio::{
    Audio, AudioAction, AudioError, AudioErrorContext, AudioInitializationAction, AudioLib,
    AudioLoadResult, AudioPlayOptions, AudioPlayResult, AudioSetupOptions, AudioSound,
    AudioStartOptions, AudioVoice, HeadlessAudioLib, AUDIO_DEFAULT_SAMPLE_RATE, AUDIO_MAX_VOICES,
    AUDIO_STATUS_ERR_DECODE, AUDIO_STATUS_ERR_DEVICE, AUDIO_STATUS_ERR_INVALID,
    AUDIO_STATUS_ERR_NOT_FOUND, AUDIO_STATUS_ERR_NO_SPACE,
};

// ---------------------------------------------------------------------------
// upstream fixtures (audio.test.ts:24-66)
// ---------------------------------------------------------------------------

const SAMPLE_RATE: u32 = 48_000;

/// `buildPcm16Wav` (audio.test.ts:27-62) — byte-for-byte.
fn build_pcm16_wav(samples: &[f64], channels: usize) -> Vec<u8> {
    assert!(channels > 0 && samples.len().is_multiple_of(channels));
    let bits_per_sample = 16u16;
    let bytes_per_sample = 2usize;
    let frame_count = samples.len() / channels;
    let data_size = frame_count * channels * bytes_per_sample;
    let byte_rate = SAMPLE_RATE as usize * channels * bytes_per_sample;
    let block_align = (channels * bytes_per_sample) as u16;
    let total_size = 44 + data_size;
    let mut out = vec![0u8; total_size];
    out[0..4].copy_from_slice(&[0x52, 0x49, 0x46, 0x46]); // RIFF
    out[4..8].copy_from_slice(&((total_size - 8) as u32).to_le_bytes());
    out[8..12].copy_from_slice(&[0x57, 0x41, 0x56, 0x45]); // WAVE
    out[12..16].copy_from_slice(&[0x66, 0x6d, 0x74, 0x20]); // "fmt "
    out[16..20].copy_from_slice(&16u32.to_le_bytes());
    out[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    out[22..24].copy_from_slice(&(channels as u16).to_le_bytes());
    out[24..28].copy_from_slice(&SAMPLE_RATE.to_le_bytes());
    out[28..32].copy_from_slice(&(byte_rate as u32).to_le_bytes());
    out[32..34].copy_from_slice(&block_align.to_le_bytes());
    out[34..36].copy_from_slice(&bits_per_sample.to_le_bytes());
    out[36..40].copy_from_slice(&[0x64, 0x61, 0x74, 0x61]); // "data"
    out[40..44].copy_from_slice(&(data_size as u32).to_le_bytes());
    for (i, sample) in samples.iter().enumerate() {
        let clamped = sample.clamp(-1.0, 1.0);
        let quantized = (clamped * 32767.0).round() as i16;
        out[44 + i * 2..46 + i * 2].copy_from_slice(&quantized.to_le_bytes());
    }
    out
}

fn build_mono_pcm16_wav(samples: &[f64]) -> Vec<u8> {
    build_pcm16_wav(samples, 1)
}

/// A synthetic MPEG-1 Layer III silent frame (44100 Hz, 128 kbps —
/// frame length 144·128000/44100 = 417 bytes, stereo → 1152 frames,
/// mono → 1152 mono samples). Original bytes built the same way a
/// muxer would; the consumer's sound assets are MP3s of exactly this
/// family (opencode `@opencode/ui/audio/*.mp3`).
fn silent_mp3_frame(channels_mode: u8) -> Vec<u8> {
    let mut frame = vec![0xffu8, 0xfb, 0x90, channels_mode << 6];
    frame.resize(417, 0);
    frame
}

// ---------------------------------------------------------------------------
// test harness (audio.test.ts's replaceMethod/lib seams, as an
// injected AudioLib — the port's equivalent of upstream monkey-
// patching the resolved render lib)
// ---------------------------------------------------------------------------

struct ScriptedLib {
    inner: HeadlessAudioLib,
    start_status: Option<i32>,
    create_fails: bool,
    destroy_fails: Arc<AtomicBool>,
    destroy_calls: Arc<AtomicUsize>,
}

impl ScriptedLib {
    fn new() -> (Self, Arc<AtomicUsize>, Arc<AtomicBool>) {
        let destroy_calls = Arc::new(AtomicUsize::new(0));
        let destroy_fails = Arc::new(AtomicBool::new(false));
        (
            Self {
                inner: HeadlessAudioLib::new(),
                start_status: None,
                create_fails: false,
                destroy_fails: destroy_fails.clone(),
                destroy_calls: destroy_calls.clone(),
            },
            destroy_calls,
            destroy_fails,
        )
    }
}

impl AudioLib for ScriptedLib {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn create_audio_engine(
        &mut self,
        options: Option<crate::audio::AudioEngineCreateOptions>,
    ) -> Option<crate::audio::AudioEngineHandle> {
        if self.create_fails {
            return None;
        }
        self.inner.create_audio_engine(options)
    }

    fn destroy_audio_engine(
        &mut self,
        engine: crate::audio::AudioEngineHandle,
    ) -> Result<(), AudioError> {
        self.destroy_calls.fetch_add(1, Ordering::SeqCst);
        if self.destroy_fails.load(Ordering::SeqCst) {
            return Err(AudioError {
                message: "scripted destroy failure".to_string(),
            });
        }
        self.inner.destroy_audio_engine(engine)
    }

    fn audio_start(
        &mut self,
        engine: crate::audio::AudioEngineHandle,
        options: Option<&AudioStartOptions>,
    ) -> i32 {
        if let Some(status) = self.start_status {
            return status;
        }
        self.inner.audio_start(engine, options)
    }

    fn audio_stop(&mut self, engine: crate::audio::AudioEngineHandle) -> i32 {
        self.inner.audio_stop(engine)
    }

    fn audio_load(
        &mut self,
        engine: crate::audio::AudioEngineHandle,
        data: &[u8],
    ) -> AudioLoadResult {
        self.inner.audio_load(engine, data)
    }

    fn audio_play(
        &mut self,
        engine: crate::audio::AudioEngineHandle,
        sound: u32,
        options: Option<crate::audio::NativeAudioVoiceOptions>,
    ) -> AudioPlayResult {
        self.inner.audio_play(engine, sound, options)
    }
}

/// The captured `(error, context)` pairs an `on("error")` listener saw
/// — upstream pushes `{ message, context }` (audio.test.ts:332-334).
#[derive(Debug, Clone, PartialEq, Eq)]
struct CapturedError {
    message: String,
    context: AudioErrorContext,
}

#[derive(Default)]
struct Captured {
    errors: Mutex<Vec<CapturedError>>,
    events: Mutex<Vec<&'static str>>,
}

impl Captured {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn attach(self: &Arc<Self>, audio: &mut Audio) {
        let for_error = self.clone();
        let for_started = self.clone();
        let for_stopped = self.clone();
        let for_disposed = self.clone();
        audio.on_error(move |error, context| {
            for_error.errors.lock().unwrap().push(CapturedError {
                message: error.message.clone(),
                context: *context,
            });
        });
        audio.on_started({
            let for_started = for_started.clone();
            move || for_started.events.lock().unwrap().push("started")
        });
        audio.on_stopped(move || for_stopped.events.lock().unwrap().push("stopped"));
        audio.on_disposed(move || for_disposed.events.lock().unwrap().push("disposed"));
    }

    fn errors(&self) -> Vec<CapturedError> {
        self.errors.lock().unwrap().clone()
    }

    fn events(&self) -> Vec<&'static str> {
        self.events.lock().unwrap().clone()
    }
}

// ---------------------------------------------------------------------------
// Audio.create / setupAudio (audio.test.ts:229-246, 342-358, 402-408)
// ---------------------------------------------------------------------------

/// audio.test.ts:229-242 — playbackChannels 0xffffffff exceeds
/// MA_MAX_CHANNELS → engine null → AudioInitializationError, no events.
#[test]
fn create_throws_synchronously_when_engine_creation_fails() {
    // captureAudioEvents (audio.test.ts:215-227) wraps create; create
    // throws before any emission (audio.ts:2804-2806) — there is no
    // instance to observe, so "no events" is structural.
    let error = Audio::create(&AudioSetupOptions {
        playback_channels: Some(0xffff_ffffu32 as f64),
        ..Default::default()
    })
    .expect_err("create must throw");
    assert_eq!(error.action, AudioInitializationAction::CreateAudioEngine);
    assert_eq!(error.message, "Audio createAudioEngine returned null");
}

/// audio.test.ts:244-246 — setupAudio shares the contract.
#[test]
fn setup_audio_uses_the_same_synchronous_initialization_contract() {
    let error = crate::audio::setup_audio(&AudioSetupOptions {
        playback_channels: Some(0xffff_ffffu32 as f64),
        ..Default::default()
    })
    .expect_err("setupAudio must throw");
    assert_eq!(error.action, AudioInitializationAction::CreateAudioEngine);
}

/// audio.test.ts:248-277 — auto-start failure throws {start, status}
/// and destroys the native engine exactly once; no events.
#[test]
fn auto_start_failure_throws_and_destroys_the_native_engine() {
    let (mut lib, destroy_calls, _destroy_fails) = ScriptedLib::new();
    lib.start_status = Some(AUDIO_STATUS_ERR_DEVICE); // replaceMethod(lib, "audioStart", () => -5)
    let thrown = Audio::create_with_lib(
        Box::new(lib),
        &AudioSetupOptions {
            auto_start: Some(true),
            ..Default::default()
        },
    )
    .expect_err("auto-start failure must throw");
    assert_eq!(thrown.action, AudioInitializationAction::Start);
    assert_eq!(thrown.status, Some(AUDIO_STATUS_ERR_DEVICE));
    assert_eq!(
        thrown.message,
        format!("Audio auto-start failed: {AUDIO_STATUS_ERR_DEVICE}")
    );
    assert_eq!(destroy_calls.load(Ordering::SeqCst), 1);
    // no events: the throw path never emits (audio.test.ts:267)
}

/// audio.test.ts:342-358 — auto-start success flips both flags
/// without emitting (events [] during create; the started event is
/// explicit-start only).
#[test]
fn auto_start_success_updates_started_state() {
    let audio = Audio::create(&AudioSetupOptions {
        auto_start: Some(true),
        ..Default::default()
    })
    .expect("null-device start succeeds");
    assert!(audio.is_started());
    assert!(audio.is_mixer_started());
}

/// audio.test.ts:402-408 — no auto-start by default.
#[test]
fn audio_does_not_auto_start_by_default() {
    let audio = Audio::create(&AudioSetupOptions::default()).expect("create succeeds");
    assert!(!audio.is_started());
    assert!(!audio.is_mixer_started());
}

/// audio.ts:2790-2794 — sampleRate normalization: default 48000,
/// custom honored, non-finite/fractional/negative fall to the default.
#[test]
fn sample_rate_normalization_matches_the_constructor() {
    // audio.test.ts:567-585 (custom) + the branch table at 2790-2794
    let default = Audio::create(&AudioSetupOptions::default()).unwrap();
    assert_eq!(default.sample_rate(), AUDIO_DEFAULT_SAMPLE_RATE);
    let custom = Audio::create(&AudioSetupOptions {
        sample_rate: Some(22_050.0),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(custom.sample_rate(), 22_050);
    // fractional truncates to 0 → default
    let fractional = Audio::create(&AudioSetupOptions {
        sample_rate: Some(0.5),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(fractional.sample_rate(), AUDIO_DEFAULT_SAMPLE_RATE);
    let negative = Audio::create(&AudioSetupOptions {
        sample_rate: Some(-44_100.0),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(negative.sample_rate(), AUDIO_DEFAULT_SAMPLE_RATE);
    let non_finite = Audio::create(&AudioSetupOptions {
        sample_rate: Some(f64::NAN),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(non_finite.sample_rate(), AUDIO_DEFAULT_SAMPLE_RATE);
}

// ---------------------------------------------------------------------------
// start (audio.test.ts:322-340, 360-376, 410-426, 952-963)
// ---------------------------------------------------------------------------

/// audio.test.ts:322-340 — a start failure reports through the error
/// contract: boolean false, message "Audio start failed: <status>",
/// context { action: "start", status }. (Upstream's −1 comes from an
/// option-packing getter throwing inside the FFI wrapper,
/// zig.ts:6394-6401 — a JS-domain artifact; the port pins the same
/// observable through an injected lib returning −1.)
#[test]
fn start_reports_a_status_failure_through_its_error_contract() {
    let (mut lib, _, _) = ScriptedLib::new();
    lib.start_status = Some(AUDIO_STATUS_ERR_INVALID);
    let mut audio = Audio::create_with_lib(Box::new(lib), &AudioSetupOptions::default()).unwrap();
    let captured = Captured::new();
    captured.attach(&mut audio);

    assert!(!audio.start(None));
    assert!(!audio.is_started());
    assert!(!audio.is_mixer_started());
    assert_eq!(
        captured.errors(),
        vec![CapturedError {
            message: "Audio start failed: -1".to_string(),
            context: AudioErrorContext {
                action: AudioAction::Start,
                status: Some(AUDIO_STATUS_ERR_INVALID),
            },
        }]
    );
}

/// audio.test.ts:360-376 — explicit start emits "started" once.
#[test]
fn explicit_start_emits_started_after_construction() {
    let mut audio = Audio::create(&AudioSetupOptions::default()).unwrap();
    let captured = Captured::new();
    captured.attach(&mut audio);

    assert!(audio.start(None));
    assert_eq!(captured.events(), vec!["started"]);
}

/// audio.test.ts:410-426 — isStarted()/isMixerStarted() track the
/// start boolean; "started" fires iff the start succeeded. The false
/// arm uses the no-device failure a real headless box produces
/// (native audioStart → err_device, audio.zig:1612-1616).
#[test]
fn start_reports_playback_availability_only() {
    // success arm (null device)
    let mut audio = Audio::create(&AudioSetupOptions::default()).unwrap();
    let captured = Captured::new();
    captured.attach(&mut audio);
    let started = audio.start(None);
    assert!(started);
    assert_eq!(audio.is_started(), started);
    assert_eq!(audio.is_mixer_started(), started);
    assert_eq!(captured.events(), vec!["started"]);

    // failure arm (no device: -5)
    let (mut lib, _, _) = ScriptedLib::new();
    lib.start_status = Some(AUDIO_STATUS_ERR_DEVICE);
    let mut audio = Audio::create_with_lib(Box::new(lib), &AudioSetupOptions::default()).unwrap();
    let captured = Captured::new();
    captured.attach(&mut audio);
    let started = audio.start(None);
    assert!(!started);
    assert_eq!(audio.is_started(), started);
    assert_eq!(audio.is_mixer_started(), started);
    assert!(captured.events().is_empty());
    assert_eq!(
        captured.errors(),
        vec![CapturedError {
            message: "Audio start failed: -5".to_string(),
            context: AudioErrorContext {
                action: AudioAction::Start,
                status: Some(AUDIO_STATUS_ERR_DEVICE),
            },
        }]
    );
}

/// audio.ts:2833-2834 — start is idempotent-true once started
/// (no second "started" emission; upstream returns early before emit).
#[test]
fn start_when_already_started_returns_true_without_emitting() {
    let mut audio = Audio::create(&AudioSetupOptions::default()).unwrap();
    assert!(audio.start(None));
    let captured = Captured::new();
    captured.attach(&mut audio);
    assert!(audio.start(None));
    assert!(captured.events().is_empty());
}

/// audio.test.ts:952-963 — node EventEmitter semantics: an "error"
/// emission with no listener throws. The port panics with the exact
/// upstream message.
#[test]
#[should_panic(expected = "Audio start failed: -2")]
fn unhandled_error_emissions_throw() {
    let (mut lib, _, _) = ScriptedLib::new();
    lib.start_status = Some(-2);
    let mut audio = Audio::create_with_lib(Box::new(lib), &AudioSetupOptions::default()).unwrap();
    // no on_error registered
    let _ = audio.start(None);
}

// ---------------------------------------------------------------------------
// loadSound (audio.test.ts:378-387, 453-459 + native load semantics)
// ---------------------------------------------------------------------------

/// audio.test.ts:378-387 — a WAV payload decodes and returns a handle.
#[test]
fn loads_wav_and_returns_a_sound_handle() {
    let mut audio = Audio::create(&AudioSetupOptions::default()).unwrap();
    let wav = build_mono_pcm16_wav(&[0.0, 0.25, -0.25, 0.5, -0.5, 0.0]);
    let sound = audio.load_sound(&wav);
    assert_eq!(sound, Some(AudioSound(1))); // 1-based (native audio.zig:2110-2117)
                                            // decoded shape: 6 mono frames at the source rate (dr_wav semantics)
    let engine = audio.engine_handle();
    let shape = audio
        .lib::<HeadlessAudioLib>()
        .and_then(|lib| lib.sound_shape(engine, 1));
    assert_eq!(shape, Some((6, 1, SAMPLE_RATE)));
}

/// The consumer's format — MP3 (opencode feeds `*.mp3` assets;
/// upstream decodes with dr_mp3, which is minimp3 — same decoder).
#[test]
fn loads_mp3_frames_like_the_consumer_assets() {
    let mut audio = Audio::create(&AudioSetupOptions::default()).unwrap();
    let mut mp3 = silent_mp3_frame(0); // stereo frame
    mp3.extend_from_within(0..417); // two frames
    let sound = audio.load_sound(&mp3);
    assert_eq!(sound, Some(AudioSound(1)));
    let engine = audio.engine_handle();
    let shape = audio
        .lib::<HeadlessAudioLib>()
        .and_then(|lib| lib.sound_shape(engine, 1));
    // 2 × 1152 stereo frames at 44100 (minimp3 output)
    assert_eq!(shape, Some((2 * 1152 * 2, 2, 44_100)));
}

/// Native `load` (audio.zig:2101-2103): empty payload → err_invalid.
#[test]
fn load_sound_rejects_empty_payloads_with_err_invalid() {
    let mut audio = Audio::create(&AudioSetupOptions::default()).unwrap();
    let captured = Captured::new();
    captured.attach(&mut audio);
    assert_eq!(audio.load_sound(&[]), None);
    assert_eq!(
        captured.errors(),
        vec![CapturedError {
            message: "Audio loadSound failed: -1".to_string(),
            context: AudioErrorContext {
                action: AudioAction::LoadSound,
                status: Some(AUDIO_STATUS_ERR_INVALID),
            },
        }]
    );
}

/// Native decode failure (audio.zig:2104): err_decode for garbage and
/// for FLAC (narrowed — upstream's dr_flac decodes it; the port
/// reports the same failure every unsupported format gets).
#[test]
fn load_sound_reports_err_decode_for_garbage_and_flac() {
    let mut audio = Audio::create(&AudioSetupOptions::default()).unwrap();
    let captured = Captured::new();
    captured.attach(&mut audio);

    assert_eq!(audio.load_sound(&[0x13, 0x37, 0x42, 0xff]), None);
    let mut flac = b"fLaC".to_vec();
    flac.extend_from_slice(&[0; 64]);
    assert_eq!(audio.load_sound(&flac), None);
    assert_eq!(
        captured.errors(),
        vec![
            CapturedError {
                message: "Audio loadSound failed: -3".to_string(),
                context: AudioErrorContext {
                    action: AudioAction::LoadSound,
                    status: Some(AUDIO_STATUS_ERR_DECODE),
                },
            };
            2
        ]
    );
}

/// WAV breadth of the port's decoder: stereo s16 (the dr_wav path the
/// upstream fixtures exercise) and IEEE float.
#[test]
fn decodes_stereo_pcm16_and_ieee_float_wav() {
    let mut audio = Audio::create(&AudioSetupOptions::default()).unwrap();

    // stereo interleave (audio.test.ts:27-62 generalized to channels=2)
    let stereo = build_pcm16_wav(&[0.25, -0.25, 0.5, -0.5], 2);
    assert_eq!(audio.load_sound(&stereo), Some(AudioSound(1)));
    let engine = audio.engine_handle();
    let shape = audio
        .lib::<HeadlessAudioLib>()
        .and_then(|lib| lib.sound_shape(engine, 1));
    assert_eq!(shape, Some((4, 2, SAMPLE_RATE)));

    // hand-rolled IEEE-f32 WAV: fmt format=3, bits=32
    let mut f32_wav = Vec::new();
    f32_wav.extend_from_slice(b"RIFF");
    f32_wav.extend_from_slice(&36u32.to_le_bytes()); // patched below with data
    f32_wav.extend_from_slice(b"WAVE");
    f32_wav.extend_from_slice(b"fmt ");
    f32_wav.extend_from_slice(&16u32.to_le_bytes());
    f32_wav.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    f32_wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    f32_wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    f32_wav.extend_from_slice(&(SAMPLE_RATE * 4).to_le_bytes());
    f32_wav.extend_from_slice(&4u16.to_le_bytes());
    f32_wav.extend_from_slice(&32u16.to_le_bytes());
    f32_wav.extend_from_slice(b"data");
    f32_wav.extend_from_slice(&16u32.to_le_bytes());
    for sample in [0.25f32, -0.5, 0.75, -1.0] {
        f32_wav.extend_from_slice(&sample.to_le_bytes());
    }
    let total = (f32_wav.len() - 8) as u32;
    f32_wav[4..8].copy_from_slice(&total.to_le_bytes());
    assert_eq!(audio.load_sound(&f32_wav), Some(AudioSound(2)));
    let shape = audio
        .lib::<HeadlessAudioLib>()
        .and_then(|lib| lib.sound_shape(engine, 2));
    assert_eq!(shape, Some((4, 1, SAMPLE_RATE)));
}

/// Sound ids are monotonic and never reused (native `load` appends;
/// ids stay valid across later loads).
#[test]
fn sound_ids_are_monotonic() {
    let mut audio = Audio::create(&AudioSetupOptions::default()).unwrap();
    let wav = build_mono_pcm16_wav(&[0.0, 0.5]);
    assert_eq!(audio.load_sound(&wav), Some(AudioSound(1)));
    assert_eq!(audio.load_sound(&wav), Some(AudioSound(2)));
    assert_eq!(audio.load_sound(&wav), Some(AudioSound(3)));
}

// ---------------------------------------------------------------------------
// play (audio.ts:2954-2976 + native play, audio.zig:2196-2243)
// ---------------------------------------------------------------------------

/// audio.test.ts:391 — play returns a voice handle for a loaded sound.
#[test]
fn play_returns_a_voice_handle() {
    let mut audio = Audio::create(&AudioSetupOptions::default()).unwrap();
    let sound = audio
        .load_sound(&build_mono_pcm16_wav(&[0.6, -0.2, 0.4]))
        .unwrap();
    assert!(!audio.is_started());
    // play does not require start (noDevice engine mixes on demand —
    // the consumer starts first, but upstream play() itself does not)
    let voice = audio.play(sound, None);
    assert_eq!(voice, Some(AudioVoice(1))); // 1-based slot (audio.zig:2241)
    let second = audio.play(sound, None);
    assert_eq!(second, Some(AudioVoice(2)));
}

/// Native play (audio.zig:2204): unknown sound → err_not_found.
#[test]
fn play_of_an_unknown_sound_reports_err_not_found() {
    let mut audio = Audio::create(&AudioSetupOptions::default()).unwrap();
    let captured = Captured::new();
    captured.attach(&mut audio);
    assert_eq!(audio.play(AudioSound(9), None), None);
    assert_eq!(
        captured.errors(),
        vec![CapturedError {
            message: "Audio play failed: -4".to_string(),
            context: AudioErrorContext {
                action: AudioAction::Play,
                status: Some(AUDIO_STATUS_ERR_NOT_FOUND),
            },
        }]
    );
}

/// Native play (audio.zig:2211): unknown group → err_invalid (group 0
/// is the implicit "default" group, audio.zig:1226).
#[test]
fn play_with_an_unknown_group_reports_err_invalid() {
    let mut audio = Audio::create(&AudioSetupOptions::default()).unwrap();
    let captured = Captured::new();
    captured.attach(&mut audio);
    let sound = audio.load_sound(&build_mono_pcm16_wav(&[0.5])).unwrap();
    let options = AudioPlayOptions {
        group_id: Some(7),
        ..Default::default()
    };
    assert_eq!(audio.play(sound, Some(&options)), None);
    assert_eq!(
        captured.errors(),
        vec![CapturedError {
            message: "Audio play failed: -1".to_string(),
            context: AudioErrorContext {
                action: AudioAction::Play,
                status: Some(AUDIO_STATUS_ERR_INVALID),
            },
        }]
    );
}

/// Native play (audio.zig:2212, 2222): the 33rd concurrent voice
/// reports err_no_space (max_voices = 32; with the null sink nothing
/// reaps, exactly like an un-mixed noDevice engine).
#[test]
fn play_exhausts_the_32_voice_budget_then_reports_err_no_space() {
    let mut audio = Audio::create(&AudioSetupOptions::default()).unwrap();
    let captured = Captured::new();
    captured.attach(&mut audio);
    let sound = audio
        .load_sound(&build_mono_pcm16_wav(&[0.5, 0.5, 0.5, 0.5]))
        .unwrap();
    for expected in 1..=AUDIO_MAX_VOICES as u32 {
        assert_eq!(audio.play(sound, None), Some(AudioVoice(expected)));
    }
    assert_eq!(audio.play(sound, None), None);
    assert_eq!(
        captured.errors(),
        vec![CapturedError {
            message: "Audio play failed: -2".to_string(),
            context: AudioErrorContext {
                action: AudioAction::Play,
                status: Some(AUDIO_STATUS_ERR_NO_SPACE),
            },
        }]
    );
}

/// Voice options resolve exactly like the FFI packer: explicit values
/// win, undefined fields pack to volume=1/pan=0/loop=false/group=0
/// (zig-structs.ts:505-510; native defaults audio.zig:2208-2212), and
/// volume/pan clamp to [0,4] / [-1,1] at the engine (audio.zig:2234).
#[test]
fn voice_options_resolve_pack_defaults_and_engine_clamps() {
    let mut audio = Audio::create(&AudioSetupOptions::default()).unwrap();
    let sound = audio.load_sound(&build_mono_pcm16_wav(&[0.5])).unwrap();
    let engine = audio.engine_handle();

    // no options at all → native defaults
    audio.play(sound, None).unwrap();
    assert_eq!(
        audio
            .lib::<HeadlessAudioLib>()
            .and_then(|lib| lib.voice_options(engine, 1)),
        Some(crate::audio::NativeAudioVoiceOptions {
            volume: 1.0,
            pan: 0.0,
            loop_: false,
            group_id: 0,
        })
    );

    // partial options → pack defaults fill the rest (volume stays 1 —
    // undefined does NOT pack as 0)
    let partial = AudioPlayOptions {
        loop_: Some(true),
        ..Default::default()
    };
    audio.play(sound, Some(&partial)).unwrap();
    assert_eq!(
        audio
            .lib::<HeadlessAudioLib>()
            .and_then(|lib| lib.voice_options(engine, 2)),
        Some(crate::audio::NativeAudioVoiceOptions {
            volume: 1.0,
            pan: 0.0,
            loop_: true,
            group_id: 0,
        })
    );

    // engine clamps (native clamp, audio.zig:2236-2238)
    let clamped = AudioPlayOptions {
        volume: Some(9.0),
        pan: Some(-3.0),
        ..Default::default()
    };
    audio.play(sound, Some(&clamped)).unwrap();
    assert_eq!(
        audio
            .lib::<HeadlessAudioLib>()
            .and_then(|lib| lib.voice_options(engine, 3)),
        Some(crate::audio::NativeAudioVoiceOptions {
            volume: 4.0,
            pan: -1.0,
            loop_: false,
            group_id: 0,
        })
    );
}

// ---------------------------------------------------------------------------
// the null sink (port-level: HeadlessAudioLib::render_frames — no
// upstream Audio-level counterpart; mixFrames is tier-2 unported)
// ---------------------------------------------------------------------------

#[test]
fn null_sink_mixes_voices_applies_volume_and_reaps_finished_voices() {
    let mut audio = Audio::create(&AudioSetupOptions::default()).unwrap();
    let sound = audio
        .load_sound(&build_mono_pcm16_wav(&[0.5, 0.5, 0.5, 0.5]))
        .unwrap();
    let engine = audio.engine_handle();
    audio.play(sound, None).unwrap();

    {
        let lib = audio.lib_mut::<HeadlessAudioLib>().unwrap();
        let mixed = lib.render_frames(engine, 2, 2).unwrap();
        // mono upmix to stereo at full gain (balance law center)
        assert_eq!(mixed.len(), 4);
        for ch in &mixed {
            assert!((ch - 0.5).abs() < 1e-6, "mixed {mixed:?}");
        }
    }
    // 4-frame sound fully consumed by two 2-frame renders → voice
    // reaped; a third render is silent (native reapFinishedVoices,
    // audio.zig:1241-1252)
    {
        let lib = audio.lib_mut::<HeadlessAudioLib>().unwrap();
        let _ = lib.render_frames(engine, 2, 2);
        let silent = lib.render_frames(engine, 2, 2).unwrap();
        assert!(silent.iter().all(|s| *s == 0.0));
        assert_eq!(lib.voices_active(engine), Some(0));
    }

    // volume scales the same sound; loop wraps instead of reaping
    audio
        .play(
            sound,
            Some(&AudioPlayOptions {
                volume: Some(2.0),
                loop_: Some(true),
                ..Default::default()
            }),
        )
        .unwrap();
    {
        let lib = audio.lib_mut::<HeadlessAudioLib>().unwrap();
        let _ = lib.render_frames(engine, 6, 2); // > sound length, loops
        let mixed = lib.render_frames(engine, 1, 2).unwrap();
        assert!((mixed[0] - 1.0).abs() < 1e-6, "volume 2 → {mixed:?}");
        assert_eq!(lib.voices_active(engine), Some(1)); // still looping
    }
}

// ---------------------------------------------------------------------------
// stop / dispose (audio.ts:2869-2885, 3612-3683; audio.test.ts:2162+)
// ---------------------------------------------------------------------------

/// audio.ts:2883 + 3656-3658 — dispose stops a started mixer first
/// (emitting "stopped"), then destroys the engine and emits
/// "disposed"; the destroyed engine surfaces as "engine unavailable"
/// on every later call (audio.ts:2837, 2898, 2966).
#[test]
fn dispose_stops_the_mixer_then_destroys_and_emits_disposed() {
    let mut audio = Audio::create(&AudioSetupOptions::default()).unwrap();
    let captured = Captured::new();
    captured.attach(&mut audio);
    assert!(audio.start(None));
    assert_eq!(captured.events(), vec!["started"]);

    audio.dispose().expect("dispose succeeds");
    assert_eq!(captured.events(), vec!["started", "stopped", "disposed"]);
    assert!(!audio.is_started());
    assert!(!audio.is_mixer_started());

    // post-dispose: engine unavailable paths (audio.ts:2835-2839,
    // 2896-2900, 2964-2968)
    assert!(!audio.start(None));
    assert_eq!(audio.load_sound(&build_mono_pcm16_wav(&[0.5])), None);
    let sound = AudioSound(1);
    assert_eq!(audio.play(sound, None), None);
    let errors: Vec<String> = captured
        .errors()
        .iter()
        .map(|CapturedError { message, .. }| message.clone())
        .collect();
    assert_eq!(
        errors,
        vec![
            "Audio engine unavailable during start",
            "Audio engine unavailable during loadSound",
            "Audio engine unavailable during play",
        ]
    );
}

/// audio.ts:3613 — dispose on a disposed Audio is a no-op (no second
/// event, no second destroy).
#[test]
fn dispose_is_idempotent() {
    let (lib, destroy_calls, _destroy_fails) = ScriptedLib::new();
    let mut audio = Audio::create_with_lib(Box::new(lib), &AudioSetupOptions::default()).unwrap();
    audio.dispose().unwrap();
    audio.dispose().unwrap();
    assert_eq!(destroy_calls.load(Ordering::SeqCst), 1);
}

/// audio.test.ts:2185-2203 — when native destruction fails, dispose
/// retains the engine (so cleanup can retry) and reports the error;
/// a later dispose completes and emits "disposed".
#[test]
fn dispose_retains_the_engine_when_native_destruction_fails() {
    let (lib, destroy_calls, destroy_fails) = ScriptedLib::new();
    let mut audio = Audio::create_with_lib(Box::new(lib), &AudioSetupOptions::default()).unwrap();
    let captured = Captured::new();
    captured.attach(&mut audio);
    assert!(audio.start(None));

    destroy_fails.store(true, Ordering::SeqCst);
    let error = audio.dispose().expect_err("destroy failure surfaces");
    assert_eq!(error.message, "scripted destroy failure");
    assert!(captured.events().iter().all(|e| *e != "disposed"));
    // engine retained → members still see an engine. The internal
    // stop() already reset the flags, so start() runs again for real
    // (and succeeds on the scripted lib's healthy default path).
    assert!(audio.start(None));

    destroy_fails.store(false, Ordering::SeqCst);
    audio.dispose().expect("retry succeeds");
    assert!(captured.events().contains(&"disposed"));
    assert_eq!(destroy_calls.load(Ordering::SeqCst), 2);
}

// ---------------------------------------------------------------------------
// the consumer contract (opencode v2 — packages/tui/src/audio.ts)
// ---------------------------------------------------------------------------

/// The exact flow opencode's TUI runs: catch create failure → degrade
/// to null (audio.ts:16-19); lazy start on play (audio.ts:41); sound
/// cache keyed by file; dispose resets everything (audio.ts:45-49).
#[test]
fn opencode_tui_consumer_flow() {
    // -- the no-audio-device arm: create itself failing (tui audio.ts:16-19)
    let (mut lib, _, _) = ScriptedLib::new();
    lib.create_fails = true;
    let audio = Audio::create_with_lib(
        Box::new(lib),
        &AudioSetupOptions {
            auto_start: Some(false),
            ..Default::default()
        },
    );
    // console.debug("failed to create tui audio") — degrade to null
    let audio: Option<Audio> = audio.ok();
    if audio.is_none() {
        // loadSoundFile resolves null and play returns null
        // (tui audio.ts:24-25, 39-42)
        return;
    }

    // -- the normal arm (headless null device)
    let mut audio = Audio::create(&AudioSetupOptions {
        auto_start: Some(false),
        ..Default::default()
    })
    .expect("tui creates with autoStart: false");
    let captured = Captured::new();
    captured.attach(&mut audio); // next.on("error", …) — tui audio.ts:11
    assert!(!audio.is_started());

    // loadSoundFile → readFile → loadSound (tui audio.ts:28-29)
    let bytes = build_mono_pcm16_wav(&[0.25, 0.5, 0.75]);
    let sound = audio.load_sound(&bytes).expect("sound decodes");
    // play(): !isStarted() && !start() → null; else play (tui audio.ts:41-42)
    let voice = if !audio.is_started() && !audio.start(None) {
        None
    } else {
        audio.play(sound, None)
    };
    assert!(audio.is_started());
    assert!(voice.is_some());
    assert!(captured.errors().is_empty());

    // dispose(): audio?.dispose(); sounds.clear() (tui audio.ts:45-49)
    audio.dispose().unwrap();
    assert_eq!(captured.events(), vec!["started", "stopped", "disposed"]);
}
