//! The miniaudio FFI boundary — the vendored header compiled by
//! `build.rs` from `native/opentui_audio.c` (the same
//! `third_party/opentui/.../vendor/miniaudio/miniaudio.h` upstream's
//! own `miniaudio_shim.c` builds). All struct layouts stay on the C
//! side of the seam; Rust sees only scalar/pointer signatures.
//!
//! - [`decode_flac`] (always compiled): `ma_dr_flac_open_memory_and_
//!   read_pcm_frames_f32` — dr_flac, the exact decoder native
//!   `decodeSoundFromMemory` reaches through miniaudio's built-in
//!   FLAC decoder (audio.zig:1198-1216).
//! - the device wrapper (only with `--features device-audio`, see
//!   `native/opentui_audio.c`): `opentui_audio_device_start` /
//!   `opentui_audio_device_destroy`.

use std::ffi::c_void;

// ---------------------------------------------------------------------------
// dr_flac — miniaudio's built-in FLAC decoder (MA_API, miniaudio.h
// 60995/90577 area; enabled unless MA_NO_FLAC, which neither flavor
// defines)
// ---------------------------------------------------------------------------

extern "C" {
    #[link_name = "ma_dr_flac_open_memory_and_read_pcm_frames_f32"]
    fn dr_flac_open_memory_and_read_pcm_frames_f32(
        data: *const c_void,
        data_size: usize,
        channels: *mut u32,
        sample_rate: *mut u32,
        total_pcm_frame_count: *mut u64,
        allocation_callbacks: *mut c_void,
    ) -> *mut f32;

    #[link_name = "ma_dr_flac_free"]
    fn dr_flac_free(pointer: *mut c_void, allocation_callbacks: *mut c_void);
}

/// Decode a `fLaC` payload to interleaved f32 at the source's native
/// channel count / sample rate — dr_flac's s16→f32 quantization
/// (`i16 / 32768`), matching how the MP3 path keeps minimp3's i16
/// output. Returns `None` on any dr_flac open/decode failure (the
/// `err_decode` input).
pub(super) fn decode_flac(bytes: &[u8]) -> Option<(Vec<f32>, u32, u32)> {
    if bytes.len() < 4 || &bytes[0..4] != b"fLaC" {
        return None;
    }
    // SAFETY: dr_flac reads `bytes` for the duration of the call and
    // returns a C-malloc'd buffer we copy out of before freeing.
    unsafe {
        let mut channels = 0u32;
        let mut sample_rate = 0u32;
        let mut total_frames = 0u64;
        let pointer = dr_flac_open_memory_and_read_pcm_frames_f32(
            bytes.as_ptr().cast(),
            bytes.len(),
            &mut channels,
            &mut sample_rate,
            &mut total_frames,
            std::ptr::null_mut(),
        );
        if pointer.is_null() || channels == 0 || sample_rate == 0 {
            if !pointer.is_null() {
                dr_flac_free(pointer.cast(), std::ptr::null_mut());
            }
            return None;
        }
        let sample_count = total_frames.checked_mul(u64::from(channels))? as usize;
        let decoded = std::slice::from_raw_parts(pointer, sample_count).to_vec();
        dr_flac_free(pointer.cast(), std::ptr::null_mut());
        Some((decoded, channels, sample_rate))
    }
}

// ---------------------------------------------------------------------------
// playback device — `device-audio` feature only (the native `start`
// device path, audio.zig:1596-1683)
// ---------------------------------------------------------------------------

#[cfg(feature = "device-audio")]
mod device {
    use std::ffi::c_void;

    /// Opaque `opentui_audio_device` (ma_context + ma_device + the
    /// Rust callback/user pair) — native/opentui_audio.c.
    #[repr(C)]
    pub struct OpaqueAudioDevice {
        _private: [u8; 0],
    }

    /// The data callback: interleaved f32 output, `frame_count *
    /// channels` samples (native audioCallback, audio.zig:2447).
    pub type DeviceDataCallback =
        unsafe extern "C" fn(user: *mut c_void, out: *mut f32, frame_count: u32, channels: u32);

    extern "C" {
        /// Returns 0 on success, −1 on any context/device init or
        /// start failure (the `err_device` equivalent — full rollback
        /// happens inside the shim). `*channels_out` receives the
        /// device's actual channel count (audio.zig:1660).
        pub fn opentui_audio_device_start(
            device_out: *mut *mut OpaqueAudioDevice,
            channels_out: *mut u32,
            sample_rate: u32,
            channels: u32,
            period_size_in_frames: u32,
            period_size_in_milliseconds: u32,
            periods: u32,
            performance_profile: u8,
            share_mode: u8,
            no_pre_silenced_output_buffer: i32,
            no_clip: i32,
            no_disable_denormals: i32,
            no_fixed_sized_callback: i32,
            wasapi_no_auto_convert_src: i32,
            wasapi_no_default_quality_src: i32,
            alsa_no_mmap: i32,
            alsa_no_auto_format: i32,
            alsa_no_auto_channels: i32,
            alsa_no_auto_resample: i32,
            user_data: *mut c_void,
            callback: DeviceDataCallback,
        ) -> i32;

        /// ma_device_stop + ma_device_uninit + context teardown
        /// (native `stop`, audio.zig:1692-1704).
        pub fn opentui_audio_device_destroy(device: *mut OpaqueAudioDevice);
    }
}

#[cfg(feature = "device-audio")]
pub(super) use device::*;
