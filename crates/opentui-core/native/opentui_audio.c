/**
 * opentui_audio.c — compiles the vendored miniaudio (read-only at
 * third_party/opentui/packages/native/src/vendor/miniaudio/miniaudio.h,
 * the exact build upstream's own miniaudio_shim.c compiles) into this
 * crate, plus a small C-boundary wrapper for the playback device so
 * Rust never has to reproduce ma_device/ma_context struct layouts.
 *
 * Two build flavors (selected by build.rs via -DOPENTUI_DEVICE_AUDIO,
 * driven by the crate's `device-audio` cargo feature):
 *
 *  - default: MA_NO_DEVICE_IO — only the built-in decoders (dr_flac)
 *    are compiled. No device/back-end code, no ALSA/Pulse headers.
 *  - device-audio: the same backend set as upstream's
 *    miniaudio_shim.c (ALSA+Pulse on Linux, CoreAudio on macOS,
 *    WASAPI/DSOUND/WINMM on Windows).
 */
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#ifdef OPENTUI_DEVICE_AUDIO
/* mirror third_party/opentui/packages/native/src/miniaudio_shim.c */
#if defined(__linux__)
#define MA_ENABLE_ONLY_SPECIFIC_BACKENDS
#define MA_ENABLE_ALSA
#define MA_ENABLE_PULSEAUDIO
#endif
#if defined(__APPLE__)
#define MA_ENABLE_ONLY_SPECIFIC_BACKENDS
#define MA_ENABLE_COREAUDIO
#endif
#if defined(_WIN32)
#define MA_ENABLE_ONLY_SPECIFIC_BACKENDS
#define MA_ENABLE_WASAPI
#define MA_ENABLE_DSOUND
#define MA_ENABLE_WINMM
#endif
#else
#define MA_NO_DEVICE_IO
#endif

#define MINIAUDIO_IMPLEMENTATION
#include "miniaudio.h"

#ifdef OPENTUI_DEVICE_AUDIO

/**
 * The data callback Rust installs (mirrors native audioCallback,
 * audio.zig:2447): receives the engine user pointer, an interleaved
 * f32 output buffer, and the device's channel count.
 */
typedef void (*opentui_audio_data_callback)(void* pUserData,
                                            float* pFramesOut,
                                            uint32_t frameCount,
                                            uint32_t channels);

typedef struct {
    ma_context context;
    ma_device device;
    int hasContext;
    void* pUserData;
    opentui_audio_data_callback callback;
} opentui_audio_device;

static ma_bool8 opentui_to_bool8(int value)
{
    return value ? MA_TRUE : MA_FALSE;
}

static ma_bool32 opentui_to_bool32(int value)
{
    return value ? MA_TRUE : MA_FALSE;
}

/* native toPerformanceProfile (audio.zig:534-539) */
static ma_performance_profile opentui_to_performance_profile(uint8_t value)
{
    if (value == 1) {
        return ma_performance_profile_conservative;
    }
    return ma_performance_profile_low_latency;
}

/* native toShareMode (audio.zig:541-546) */
static ma_share_mode opentui_to_share_mode(uint8_t value)
{
    if (value == 1) {
        return ma_share_mode_exclusive;
    }
    return ma_share_mode_shared;
}

/* native audioCallback (audio.zig:2447-2508), narrowed to the shim
 * boundary: null user/callback silences the buffer (audio.zig:2459-2462). */
static void opentui_data_callback(ma_device* pDevice, void* pOutput, const void* pInput, ma_uint32 frameCount)
{
    opentui_audio_device* d;
    (void)pInput;
    d = (opentui_audio_device*)pDevice->pUserData;
    if (d == NULL || d->callback == NULL || pOutput == NULL) {
        if (pOutput != NULL) {
            memset(pOutput, 0, (size_t)frameCount * pDevice->playback.channels * sizeof(float));
        }
        return;
    }
    d->callback(d->pUserData, (float*)pOutput, frameCount, pDevice->playback.channels);
}

/**
 * Open + start a playback device — native `start` (audio.zig:1596-1683):
 * ensureContextInitialized (ma_context_init, audio.zig:1002-1011),
 * ma_device_config_init(ma_device_type_playback) with f32 / engine
 * channels / engine sample rate + the StartOptions fields, then
 * ma_device_init and ma_device_start. Any failure at any stage rolls
 * the whole device back and reports the err_device equivalent (-1).
 * On success writes the device's actual channel count (audio.zig:1660)
 * to *pChannelsOut.
 */
int opentui_audio_device_start(opentui_audio_device** ppDeviceOut,
                               uint32_t* pChannelsOut,
                               uint32_t sampleRate,
                               uint32_t channels,
                               uint32_t periodSizeInFrames,
                               uint32_t periodSizeInMilliseconds,
                               uint32_t periods,
                               uint8_t performanceProfile,
                               uint8_t shareMode,
                               int noPreSilencedOutputBuffer,
                               int noClip,
                               int noDisableDenormals,
                               int noFixedSizeCallback,
                               int wasapiNoAutoConvertSRC,
                               int wasapiNoDefaultQualitySRC,
                               int alsaNoMMap,
                               int alsaNoAutoFormat,
                               int alsaNoAutoChannels,
                               int alsaNoAutoResample,
                               void* pUserData,
                               opentui_audio_data_callback callback)
{
    ma_result result;
    opentui_audio_device* d;
    ma_device_config config;

    if (ppDeviceOut == NULL) {
        return -1;
    }
    *ppDeviceOut = NULL;
    if (pChannelsOut != NULL) {
        *pChannelsOut = 0;
    }

    d = (opentui_audio_device*)malloc(sizeof(*d));
    if (d == NULL) {
        return -1;
    }
    memset(d, 0, sizeof(*d));
    d->pUserData = pUserData;
    d->callback = callback;

    result = ma_context_init(NULL, 0, NULL, &d->context);
    if (result != MA_SUCCESS) {
        free(d);
        return -1;
    }
    d->hasContext = 1;

    config = ma_device_config_init(ma_device_type_playback);
    config.sampleRate = sampleRate;
    config.periodSizeInFrames = periodSizeInFrames;
    config.periodSizeInMilliseconds = periodSizeInMilliseconds;
    config.periods = periods;
    config.performanceProfile = opentui_to_performance_profile(performanceProfile);
    config.noPreSilencedOutputBuffer = opentui_to_bool8(noPreSilencedOutputBuffer);
    config.noClip = opentui_to_bool8(noClip);
    config.noDisableDenormals = opentui_to_bool8(noDisableDenormals);
    config.noFixedSizedCallback = opentui_to_bool8(noFixedSizeCallback);
    config.playback.format = ma_format_f32;
    config.playback.channels = channels;
    config.playback.shareMode = opentui_to_share_mode(shareMode);
    config.playback.pDeviceID = NULL;
    config.wasapi.noAutoConvertSRC = opentui_to_bool8(wasapiNoAutoConvertSRC);
    config.wasapi.noDefaultQualitySRC = opentui_to_bool8(wasapiNoDefaultQualitySRC);
    config.alsa.noMMap = opentui_to_bool32(alsaNoMMap);
    config.alsa.noAutoFormat = opentui_to_bool32(alsaNoAutoFormat);
    config.alsa.noAutoChannels = opentui_to_bool32(alsaNoAutoChannels);
    config.alsa.noAutoResample = opentui_to_bool32(alsaNoAutoResample);
    config.dataCallback = opentui_data_callback;
    config.pUserData = d;

    result = ma_device_init(&d->context, &config, &d->device);
    if (result != MA_SUCCESS) {
        ma_context_uninit(&d->context);
        free(d);
        return -1;
    }

    /* ma_device_start failure rolls back device + context
     * (audio.zig:1672-1680; the context teardown is this wrapper's
     * ownership hygiene) */
    result = ma_device_start(&d->device);
    if (result != MA_SUCCESS) {
        ma_device_uninit(&d->device);
        ma_context_uninit(&d->context);
        free(d);
        return -1;
    }

    if (pChannelsOut != NULL) {
        *pChannelsOut = d->device.playback.channels;
    }
    *ppDeviceOut = d;
    return 0;
}

/** Native `stop` (audio.zig:1692-1704): stop + uninit, always. */
void opentui_audio_device_destroy(opentui_audio_device* d)
{
    if (d == NULL) {
        return;
    }
    ma_device_stop(&d->device);
    ma_device_uninit(&d->device);
    if (d->hasContext) {
        ma_context_uninit(&d->context);
    }
    free(d);
}

#endif /* OPENTUI_DEVICE_AUDIO */
