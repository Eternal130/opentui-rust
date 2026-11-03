# PORT-MATRIX — the frozen API surface

**Spec:** `@opentui` **0.5.14** (opencode v2.0.24's version). Source of
truth: `third_party/opentui` at tag `v0.5.14` (commit `31a93fb`,
"Release v0.5.14"), verified against the published npm tarballs
(`@opentui/core|solid|keymap@0.5.14`: package identity + symbol
spot-checks). Upstream is MIT.

## How this table was frozen

* **Runtime export enumeration** — the published packages imported under
  bun; `Object.keys(namespace)` per entry point (279 core + 48 solid +
  4/3/5 keymap subpaths = **339 runtime symbols**). Enumerated dump:
  `tools/diff/sandbox/exports.json`.
* **file:line** — located by scanning the vendored source for each
  symbol's `export` declaration. `—` = re-export/external (e.g. the
  solid-js passthroughs in @opentui/solid).
* **Tier 1 (bold)** = the symbols opencode v2.0.24's TUI actually
  imports (parsed from `packages/tui/src/**` import statements:
  79 distinct names across core/solid/keymap(+`opentui`/`solid`/`extras`/
  `addons/opentui`) subpaths). These are the port's priority order;
  everything else is tier 2.
* **status** — `done` shipped in this repo (validated per the family
  hook below); `partial` = shape/type present, semantics narrowed;
  `todo` = not yet.

## Verification hooks (per family)

| family | hook |
|---|---|
| colors (RGBA/intent/parse) | unit tests citing ansi.zig/RGBA.ts lines + differential fg/bg packed-u16 compare |
| cell grid (buffer/draw ops) | unit tests citing buffer.zig lines + differential spans (char+fg+bg+attrs per cell) |
| layout (taffy↔yoga) | differential scenarios (`tools/diff/scenarios/*.json`) vs the real opentui |
| renderables (Box/Text/…) | differential scenarios, growing per lane |
| renderer (frames/SGR) | differential + vt-replay (SGR stream repaints the grid) |
| keymap | unit tests citing keymap.ts/services/keys.ts lines (input-replay differential lanes later) |
| scrollbox/scroll | SGR scenarios vs the real ScrollBox (11 scenarios: geometry, clipping, sticky, resize, hscroll) |
| textarea/editor (O2e) | SGR scenarios vs the real textarea (12: multiline/scroll-follow/selection×3/placeholder/wrap/ops/keyboard-actions/opaque/unicode) + behavior suite (editor_tests.rs) porting upstream's edit-buffer/editor-view/keybinding tests with oracle-verified matrices |
| solid glue | consumer-side: opencode-rust screens mount via builders (their PTY goldens) |

## Stats

### Bootstrap snapshot (historical — frozen at bootstrap, kept for provenance)

The counts as of the repo's bootstrap. Later lanes updated the
per-symbol rows below, not this table; see the current table for the
recomputed state.

| package | exports | tier 1 | done | partial | todo |
|---|---|---|---|---|---|
| @opentui/core | 279 | 55 | 26 | 8 | 245 |
| @opentui/solid | 48 | 10 | 5 | 4 | 39 |
| @opentui/keymap (+subpaths) | 12 | 14* | 4 | 2 | 6 |
| **total** | **339** | **79** | **35** | **18** | **286** |

### Current (recounted 2026-11-04 via `tools/recount-matrix.sh` — run it after every lane that edits rows)

| package | exports | done | partial | deferred | todo |
|---|---|---|---|---|---|
| @opentui/core | 279 | 124 | 15 | 0 | 140 |
| @opentui/solid | 48 | 35 | 3 | 7 | 3 |
| @opentui/keymap (+subpaths) | 12 | 12 | 0 | 0 | 0 |
| **total** | **339** | **171 (50.4%)** | **18** | **7** | **143** |
| tier-1 rows only (bold `**1**`) | 52 | 45 | 7 | 0 | 0 |

Audio lane note (the tier-1 `partial` — last tier-1 row landed): the
`Audio` row is the *consumer contract* opencode v2's TUI uses
(`packages/tui/src/audio.ts`, the only consumer), ported 1:1 over an
injectable `AudioLib` trait (the render-lib seam upstream's own tests
monkey-patch). Semantics are real: MP3 decode via `minimp3` — the
same decoder upstream's miniaudio uses (`dr_mp3` builds on minimp3) —
plus RIFF/WAVE PCM/IEEE-float; native status codes (−1 invalid / −2
no-space / −3 decode / −4 not-found / −5 device), 1-based monotonic
sound ids, the 32-voice budget, volume `[0,4]` / pan `[-1,1]` clamps,
the implicit default group, node-EventEmitter error semantics
(unhandled "error" throws — audio.test.ts:952), and dispose's
stop→destroy→disposed ladder incl. engine retention on destroy
failure. Narrowed exactly: (1) the default backend is a **null
in-memory sink** — no audio device I/O exists in a headless crate
(mixing is observable via `HeadlessAudioLib::render_frames`, never
audible; the −5 no-device start failure is pinned via an injected
lib, as upstream's tests do); (2) FLAC reports `err_decode`
(upstream's dr_flac decodes it); (3) `AudioStartOptions` fields are
accepted but not applied (null device has no period config); (4) MP3
samples pass through minimp3's i16 output scaled to f32. Pinned by 27
unit tests (ports of audio.test.ts:229-426/952 + consumer-flow +
native-state-machine pins). No `tools/diff` scenario: audio output is
not cell-grid observable — the differential harness compares rendered
grids, and `Audio` emits no cells; the decode/state-machine contract
is pinned by the unit tests instead. Capture
(`openCapture`/`startCapture`/…), icy streaming (`playStream*`), the
recorder (`recordToFile`), device selection and stats stay **todo**.

Image lane note (C3 — the `ImageRenderable` + image.ts family):
observability ruling first (same shape as the audio lane): image
transport (kitty APC / sixel DCS) and blocks rasterization live in
the native `bufferDrawImage` (buffer.ts:383-432), which the Rust
port's `OptimizedBuffer` does not have — so NOTHING here is
cell-grid observable and the differential suite is unchanged (119
IDENTICAL, no new scenario); the contract is pinned instead by 20
unit tests (ports of image.test.ts / image-pool.test.ts /
image-renderable.test.ts key cases). What is real: format sniffing
+ geometry, the error-code surface (status tables 1:1), the
refcount/dispose lifecycle, pool slots, fit/aspect math, protocol
resolution, the drawImage argument tuple, and raw-rgba pixels +
their ops (resize kernels incl. Mitchell-Netravali, linear-light
composite, transforms). Narrowed exactly: (1) compressed pixel
decode and PNG re-encode report `unsupported-feature` (upstream
defers decode too — image.test.ts:713); (2) http(s)/blob URL fetch
is not ported (no network stack in a headless crate — surfaces as
`unsupported-url-scheme`); (3) ICC/cICP color status is always
`assumed-srgb` and JPEG EXIF orientation is always 1 (both ride the
decode lane); (4) loading is synchronous, so AbortController /
in-flight-abort semantics collapse; a disposed source still reports
its error asynchronously-shaped (`onError` + `loadError` + no
image). The lane also closes the A1-registered teardown gap:
`Terminal::note_kitty_graphics()` arms a
`\x1b_Ga=d,d=a\x1b\\` delete-all prologue in the teardown epilogue
(image sessions only — default builds byte-identical; upstream's
per-placement deletes at renderer.zig:602-618 /
terminal-image.zig:234, delete-all spelling per the oc-tui consumer
terminal_proto.rs:53). `TerminalCapabilities` gained the
`multiplexer` + `image_protocol` fields (terminal.zig:207/212,
types.ts:136-137) that `resolveImageRenderProtocol` reads.

Terminal backend lane note (A1-A3, feature-gated — NOT part of the
frozen 339): upstream owns terminal I/O in the Node host + zig native
lib (`terminal.zig`, `renderer.zig`, `renderer.ts`), none of which is
an exportable runtime symbol in the frozen table. The lane's
deliverables live behind the cargo feature `terminal-backend` as the
additive module `opentui-core::terminal` — `TtyDevice`/`UnixTty`
(termios raw/restore, TIOCGWINSZ), the upstream-cited mode byte
sequences, the RAII `Terminal` lifecycle (setup burst
renderer.zig:558-576, teardown epilogue renderer.zig:600-654 /
terminal.zig:282-335, panic-hook registry) + `teardown_scrollback`
(the alt-screen tail jump), the capability probe burst
(terminal.zig:347-399 incl. the kitty graphics `_Gi=31337` query)
with its streaming response classifier (terminal.zig:1247-1446), the
input/output pumps (`?2026` envelope) and resize watch, and the
`Resize`/`Capabilities` `CliRenderEvent` variants; A3 additionally
adds the OSC 8 hyperlink emission in `emit_frame_sgr`
(renderer.zig:2750-2765/2920-2925) over a renderer-side interned
link-id→URL pool. Relationship to the frozen 339 surface: purely
additive feature-gated symbols — no frozen row is replaced or
narrowed, and default (feature-off) builds are byte-identical to
pre-lane (the differential suite runs feature-off and stayed
IDENTICAL across all scenarios; OSC 8 emits only when a cell carries
a link id, which no existing scenario does). Narrowing points, all
GAPS-registered in `terminal/mod.rs`: alt-screen only (inline
viewport / split-footer / `ExternalOutputCommit` family → B4); the
kitty graphics query is sent unconditionally where upstream gates on
tmux/env overrides, and the `graphics_enabled` env override has no
port; the explicit-width/scaled-text probe block (and its CPR seed)
is deferred; notification queries (`OSC 99`/`1337`), the foot branch,
and the tmux DCS passthrough wrappers are deferred; the LinkPool's
generation/refcount slot recycling (link.zig:7-10) is simplified to
monotonic interned ids; the panic-hook epilogue writes to process
stdout (a non-stdout `W` keeps its own hook). Pinned by byte-exact
unit tests citing the upstream lines plus PTY integration tests over
a private `posix_openpt` pair.

Embedded-terminal lane note (the `partial`): upstream's class wraps a
native FFI handle backed by Ghostty's `libghostty-vt`
(`packages/native/src/embedded-terminal/`), so "done" would mean
porting a real terminal emulator. The lane shipped
`opentui-core::embedded_terminal` — a VT engine on the `vte` state
machine (already in the lockfile via the vt100 replay dependency) with
ghostty's compositor semantics (`compositor.zig`) and Ghostty's input
encoders (`src/input/{key_encode,mouse_encode,paste}.zig`) ported from
the MIT source. Observable parity is pinned: 18 `et-*` differential
scenarios (geometry, SGR/BCE styling, OSC 10/11, wrap+wide, cursor
ops, scroll+scrollback incl. reflow-on-resize, alt screen,
transparent bg, focus cursor, mouse-drag selection) + 31 unit tests
porting `EmbeddedTerminal.test.ts` — all IDENTICAL vs the pinned
0.5.14. Narrowed vs ghostty: grapheme clustering is
single-codepoint+width, tab stops are fixed at 8, the kitty key table
covers `physicalKey`'s vocabulary, and unpinned VT corners (IRM,
modifyOtherKeys state 2, DECSTBM interactions) are best-effort.

\* The tier-1 size has two readings, both correct: **79** = distinct
consumer import names parsed from opencode v2.0.24's TUI imports
across core/solid/keymap(+`opentui`/`solid`/`extras`/`addons/opentui`)
subpaths (the reading used in "How this table was frozen" above);
**52** = the bold tier-1 rows actually present in the matrix below
(39 core + 9 solid + 4 keymap). Keymap subpath symbols land in the
same Rust crate; counted once.

The tier-2 bulk falls into families that map 1:1 to the next lanes (see
README §roadmap): renderables (ScrollBox/Textarea/Input/Select/Markdown/
Code/Diff/…), events/keys/mouse parsing, post effects, audio, images,
tree-sitter styling, testing helpers.

---

### core (279 runtime exports)

| symbol | tier | upstream file:line | Rust target | status |
|---|---|---|---|---|
| `ACHROMATOPSIA_MATRIX` | 2 | core/src/post/matrices.ts:86 | opentui-core | todo |
| `ASCIIFont` | 2 | core/src/renderables/composition/constructs.ts:40 | opentui-core | todo |
| `ASCIIFontRenderable` | 2 | core/src/renderables/ASCIIFont.ts:29 | opentui-core::renderables | todo |
| `ASCIIFontSelectionHelper` | 2 | core/src/lib/selection.ts:186 | opentui-core | todo |
| `ATTRIBUTE_BASE_BITS` | 2 | core/src/types.ts:21 | opentui-core | todo |
| `ATTRIBUTE_BASE_MASK` | 2 | core/src/types.ts:22 | opentui-core | todo |
| `ArrowRenderable` | 2 | core/src/renderables/ScrollBar.ts:320 | opentui-core::renderables | done |
| `Audio` | **1** | core/src/audio.ts:2752 | opentui-core | partial (the tier-1 consumer contract — create/on("error")/loadSound/isStarted/start/play/dispose + stop·isMixerStarted — over a swappable `AudioLib` seam; native status-code/id/32-voice-cap state machine + real MP3 (minimp3, upstream's dr_mp3 engine) & WAV-PCM decode; output is a null in-memory sink (no device I/O), FLAC reports err_decode; capture/stream/recorder/device families todo) |
| `AudioCaptureStream` | 2 | core/src/audio.ts:1759 | opentui-core | todo |
| `AudioCaptureStreamError` | 2 | core/src/audio.ts:371 | opentui-core | todo |
| `AudioInitializationError` | 2 | core/src/audio.ts:358 | opentui-core | done (all three actions, status + cause chaining; pinned by audio.test.ts:229-246, 248-277 ports) |
| `AudioRecorder` | 2 | core/src/audio.ts:2249 | opentui-core | todo |
| `AudioRecorderError` | 2 | core/src/audio.ts:382 | opentui-core | todo |
| `AudioStream` | 2 | core/src/audio.ts:867 | opentui-core | todo |
| `AudioStreamError` | 2 | core/src/audio.ts:483 | opentui-core | todo |
| `BaseRenderable` | 2 | core/src/Renderable.ts:136 | opentui-core::renderables | done |
| `BloomEffect` | 2 | core/src/post/filters.ts:398 | opentui-core | todo |
| `BorderCharArrays` | 2 | core/src/lib/border.ts:165 | opentui-core | todo |
| `BorderChars` | 2 | core/src/lib/border.ts:39 | opentui-core | todo |
| `Box` | 2 | core/src/renderables/composition/constructs.ts:32 | opentui-core | done |
| `BoxRenderable` | **1** | core/src/renderables/Box.ts:46 | opentui-core::renderables | done |
| `CRTRollingBarEffect` | 2 | core/src/post/effects.ts:727 | opentui-core | todo |
| `CliRenderEvents` | **1** | core/src/renderer.ts:750 | opentui-core::renderer | partial (FRAME/FOCUS family/RESIZE/CAPABILITIES/THEME_MODE/EXTERNAL_OUTPUT (renderer.ts:755, :2608-2613 — landed D1b-core) emitted; DESTROY — renderer.ts:4408's destroy ladder — still unported with RENDER_ERROR/HANDLER_ERROR/SELECTION/DEBUG_OVERLAY_TOGGLE/MEMORY_SNAPSHOT) |
| `CliRenderer` | **1** | core/src/renderer.ts:780 | opentui-core::renderer | partial (headless render target + events/focus/mouse/input + D1a lifecycle: requestRender/settle coalescing, destroy/isDestroyed no-op semantics, setBackgroundColor, setTerminalTitle, waitForThemeMode, useMouse runtime toggle, consoleMode "disabled", session ownership (terminal-backend); screenMode/footerHeight/externalOutputMode + createScrollbackSurface/resetSplitFooterForReplay landed D1b-surface; debug overlay + kitty transport stats todo) |
| `Clipboard` | 2 | core/src/lib/clipboard.ts:337 | opentui-core | todo |
| `ClipboardTarget` | 2 | core/src/lib/clipboard.ts:292 | opentui-core | done (ABI enum; clipboard.test.ts:19-24 pinned) |
| `CloudsEffect` | 2 | core/src/post/effects.ts:494 | opentui-core | todo |
| `Code` | 2 | core/src/renderables/composition/constructs.ts:60 | opentui-core | todo |
| `CodeRenderable` | **1** | core/src/renderables/Code.ts:48 | opentui-core::renderables | done |
| `ConsolePosition` | 2 | core/src/console.ts:217 | opentui-core | todo |
| `DEFAULT_BACKGROUND_RGB` | 2 | core/src/lib/RGBA.ts:6 | opentui-core | done |
| `DEFAULT_FOREGROUND_RGB` | 2 | core/src/lib/RGBA.ts:5 | opentui-core | done |
| `DEUTERANOPIA_COMP_MATRIX` | 2 | core/src/post/matrices.ts:126 | opentui-core | todo |
| `DEUTERANOPIA_SIM_MATRIX` | 2 | core/src/post/matrices.ts:46 | opentui-core | todo |
| `DataPathsManager` | 2 | core/src/lib/data-paths.ts:34 | opentui-core | todo |
| `DebugOverlayCorner` | 2 | core/src/types.ts:92 | opentui-core | todo |
| `DiffRenderable` | **1** | core/src/renderables/Diff.ts:55 | opentui-core::renderables | done (jsdiff `parsePatch` 9.0.0 ported 1:1 in `renderables/diff.rs`; unified/split + wrap-alignment + error view; 21 diff scenarios IDENTICAL) |
| `DistortionEffect` | 2 | core/src/post/effects.ts:29 | opentui-core | todo |
| `EditBuffer` | 2 | core/src/edit-buffer.ts:19 | opentui-core::editor | done |
| `EditBufferRenderable` | 2 | core/src/renderables/EditBufferRenderable.ts:85 | opentui-core::renderables::textarea | done |
| `EditBufferRenderableEvents` | 2 | core/src/renderables/EditBufferRenderable.ts:36 | opentui-core::renderables::textarea | partial |
| `EditorView` | 2 | core/src/editor-view.ts:23 | opentui-core::editor | done |
| `EmbeddedTerminalRenderable` | **1** | core/src/renderables/EmbeddedTerminal.ts:42 | opentui-core::renderables | partial (renderable 1:1 + a vte-based engine pinned by 18 differential scenarios + 31 ported upstream tests; NOT full ghostty-vt emulation — see note) |
| `ExtmarksController` | 2 | core/src/lib/extmarks.ts:33 | opentui-core | todo |
| `FlamesEffect` | 2 | core/src/post/effects.ts:605 | opentui-core | todo |
| `FrameBuffer` | 2 | core/src/renderables/composition/constructs.ts:56 | opentui-core::buffer | todo |
| `FrameBufferRenderable` | 2 | core/src/renderables/FrameBuffer.ts:11 | opentui-core::renderables | todo |
| `GRAYSCALE_MATRIX` | 2 | core/src/post/matrices.ts:251 | opentui-core | todo |
| `GREENSCALE_MATRIX` | 2 | core/src/post/matrices.ts:231 | opentui-core | todo |
| `Generic` | 2 | core/src/renderables/composition/constructs.ts:28 | opentui-core | todo |
| `INVERT_MATRIX` | 2 | core/src/post/matrices.ts:271 | opentui-core | todo |
| `ImageError` | 2 | core/src/image.ts:173 | opentui-core | done (unified error surface: the native status payload with STATUS_MESSAGES/STATUS_CODES 1:1, image.ts:125-183; image.test.ts table arms pinned) |
| `ImageLoadError` | 2 | core/src/image.ts:16 | opentui-core | done (code/source/status/cause; bytes + filesystem paths + `data:;base64` URLs load real, oversized files reject before reading (image.ts:323); http(s)/blob fetch = `unsupported-url-scheme` — the network lane's narrowing) |
| `ImageRenderable` | 2 | core/src/renderables/Image.ts:41 | opentui-core::renderables | partial (source lifecycle/retain-dispose, fit geometry `getFittedSize`/`cellAspectRatio`, `effectiveProtocol`, and the exact `renderSelf` drawImage argument tuple (`draw_request`, Image.ts:174-219) 1:1 over `ImageRenderCtx`; SYNC port — `NativeImage` decodes at construction, so the async AbortController/fetch/abort-in-flight semantics (image-renderable.test.ts:636-785) collapse to generation no-ops; the buffer rasterization itself (`bufferDrawImage` → blocks/kitty/sixel bytes) stays with the native-render lane — NOT cell-grid observable here, pinned by image_tests.rs) |
| `Input` | 2 | core/src/renderables/composition/constructs.ts:44 | opentui-core | todo |
| `InputRenderable` | **1** | core/src/renderables/Input.ts:46 | opentui-core::input | done |
| `InputRenderableEvents` | 2 | core/src/renderables/Input.ts:29 | opentui-core::input | done |
| `InternalKeyHandler` | 2 | core/src/lib/KeyHandler.ts:134 | opentui-core::events | done |
| `KeyEvent` | **1** | core/src/lib/KeyHandler.ts:5 | opentui-core::events | done |
| `KeyHandler` | 2 | core/src/lib/KeyHandler.ts:99 | opentui-core::events | done |
| `LayoutEvents` | 2 | core/src/Renderable.ts:39 | opentui-core | todo |
| `LineNumberRenderable` | **1** | core/src/renderables/LineNumberRenderable.ts:365 | opentui-core::renderables | done (GutterRenderable incl. measure func + conceal source-map lineInfo; via Diff, 21 scenarios IDENTICAL) |
| `LinearScrollAccel` | 2 | core/src/lib/scroll-acceleration.ts:6 | opentui-core | done |
| `LogLevel` | 2 | core/src/zig.ts:2339 | opentui-core | todo |
| `MAX_LINK_URL_BYTES` | 2 | core/src/zig.ts:105 | opentui-core::ansi | todo |
| `MacOSScrollAccel` | **1** | core/src/lib/scroll-acceleration.ts:34 | opentui-core | done |
| `MarkdownRenderable` | **1** | core/src/renderables/Markdown.ts:270 | opentui-core::renderables | done |
| `MouseButton` | **1** | core/src/renderer.ts:705 | opentui-core::events | done |
| `MouseEvent` | **1** | core/src/renderer.ts:653 | opentui-core::events | done |
| `MouseParser` | 2 | core/src/lib/parse.mouse.ts:22 | opentui-core::parse_mouse | done |
| `NativeAudioStreamCloseReason` | 2 | core/src/zig.ts:97 | opentui-core | todo |
| `NativeAudioStreamFormat` | 2 | core/src/zig.ts:99 | opentui-core | todo |
| `NativeAudioStreamState` | 2 | core/src/zig.ts:95 | opentui-core | todo |
| `NativeClipboardCancelStatus` | 2 | core/src/zig.ts:161 | opentui-core | todo |
| `NativeClipboardCopyStatus` | 2 | core/src/zig.ts:167 | opentui-core | todo |
| `NativeClipboardDestroyStatus` | 2 | core/src/zig.ts:175 | opentui-core | todo |
| `NativeClipboardOperationStatus` | 2 | core/src/zig.ts:138 | opentui-core | todo |
| `NativeClipboardShutdownStatus` | 2 | core/src/zig.ts:181 | opentui-core | todo |
| `NativeClipboardStartStatus` | 2 | core/src/zig.ts:152 | opentui-core | todo |
| `NativeImage` | 2 | core/src/image.ts:383 | opentui-core | partial (real: header sniff for png/gif/jpeg/webp geometry+format (incl. animated-WebP rejection), raw-rgba/from_pixels import (stride/bgra/opaque), retain/clone/dispose refcount lifecycle, take_raw's retained-by-buffers refusal, resize (all kernels incl. Mitchell-Netravali cubics), extract/extend/rotate/flip/flop, linear-light composite, encoded-payload retention (the kitty passthrough); NARROWED: compressed pixel decode reports `unsupported-feature` until the native-render lane (upstream defers decode too, image.test.ts:713), PNG re-encode likewise, ICC/cICP color status is always assumed-srgb, JPEG EXIF orientation stays 1) |
| `NativeImagePool` | 2 | core/src/image.ts:659 | opentui-core | done (slot machine 1:1: busy = outstanding retains (status 12), in-place refresh, capacity growth 1-8 then drop, validate-before-busy-scan, dispose leaves outstanding frames alive; image-pool.test.ts:7/89/187/202 pinned) |
| `NativeMeasureTargetKind` | 2 | core/src/zig.ts:2414 | opentui-core | todo |
| `NativeSpanFeed` | 2 | core/src/NativeSpanFeed.ts:34 | opentui-core::buffer | todo |
| `OptimizedBuffer` | **1** | core/src/buffer.ts:54 | opentui-core::buffer | done |
| `PROTANOPIA_COMP_MATRIX` | 2 | core/src/post/matrices.ts:106 | opentui-core | todo |
| `PROTANOPIA_SIM_MATRIX` | 2 | core/src/post/matrices.ts:26 | opentui-core | todo |
| `PasteEvent` | **1** | core/src/lib/KeyHandler.ts:64 | opentui-core::events | done |
| `RGBA` | **1** | core/src/lib/RGBA.ts:90 | opentui-core::color | done |
| `RainbowTextEffect` | 2 | core/src/post/effects.ts:840 | opentui-core | todo |
| `Renderable` | **1** | core/src/Renderable.ts:225 | opentui-core::renderables | done |
| `RenderableEvents` | 2 | core/src/Renderable.ts:44 | opentui-core::renderables | todo |
| `RendererControlState` | 2 | core/src/renderer.ts:769 | opentui-core::renderer | todo |
| `RootRenderable` | 2 | core/src/Renderable.ts:1743 | opentui-core::renderables | done |
| `RootTextNodeRenderable` | **1** | core/src/renderables/TextNode.ts:363 | opentui-core::text_node | done |
| `SEPIA_MATRIX` | 2 | core/src/post/matrices.ts:2 | opentui-core | todo |
| `SOLARIZATION_MATRIX` | 2 | core/src/post/matrices.ts:191 | opentui-core | todo |
| `SYNTHWAVE_MATRIX` | 2 | core/src/post/matrices.ts:211 | opentui-core | todo |
| `ScrollBarRenderable` | 2 | core/src/renderables/ScrollBar.ts:20 | opentui-core::renderables | done |
| `ScrollBox` | 2 | core/src/renderables/composition/constructs.ts:64 | opentui-core | done |
| `ScrollBoxRenderable` | **1** | core/src/renderables/ScrollBox.ts:109 | opentui-core::renderables | done |
| `Select` | 2 | core/src/renderables/composition/constructs.ts:48 | opentui-core | todo |
| `SelectRenderable` | 2 | core/src/renderables/Select.ts:66 | opentui-core::renderables | todo |
| `SelectRenderableEvents` | 2 | core/src/renderables/Select.ts:61 | opentui-core::renderables | todo |
| `Selection` | 2 | core/src/lib/selection.ts:27 | opentui-core::renderer | partial |
| `SliderRenderable` | 2 | core/src/renderables/Slider.ts:20 | opentui-core::renderables | done |
| `SlotRegistry` | 2 | core/src/plugins/registry.ts:40 | opentui-core | todo |
| `SlotRenderable` | 2 | core/src/plugins/core-slot.ts:241 | opentui-core::renderables | todo |
| `StdinParser` | 2 | core/src/lib/stdin-parser.ts:587 | opentui-core::renderer | partial |
| `StyledText` | **1** | core/src/lib/styled-text.ts:26 | opentui-core::styled_text | done |
| `SyntaxStyle` | **1** | core/src/syntax-style.ts:76 | opentui-core::syntax_style | done |
| `SystemClock` | 2 | core/src/lib/clock.ts:11 | opentui-core | todo |
| `TECHNICOLOR_MATRIX` | 2 | core/src/post/matrices.ts:170 | opentui-core | todo |
| `TRITANOPIA_COMP_MATRIX` | 2 | core/src/post/matrices.ts:146 | opentui-core | todo |
| `TRITANOPIA_SIM_MATRIX` | 2 | core/src/post/matrices.ts:66 | opentui-core | todo |
| `TabSelect` | 2 | core/src/renderables/composition/constructs.ts:52 | opentui-core | todo |
| `TabSelectRenderable` | 2 | core/src/renderables/TabSelect.ts:74 | opentui-core::renderables | todo |
| `TabSelectRenderableEvents` | 2 | core/src/renderables/TabSelect.ts:55 | opentui-core::renderables | todo |
| `TargetChannel` | **1** | core/src/types.ts:99 | opentui-core | partial |
| `TerminalConsole` | 2 | core/src/console.ts:322 | opentui-core | todo |
| `TerminalPalette` | 2 | core/src/lib/terminal-palette.ts:93 | opentui-core | partial (type+construction half landed D1c: `TerminalColors` :24-35 + `normalizeTerminalPalette` :415 in `opentui-core::terminal_palette`; the OSC 4/10/11 detector class + idle timeout (:93+, :354-413) stays deferred — no ref consumer) |
| `Text` | 2 | core/src/renderables/composition/constructs.ts:36 | opentui-core | done |
| `TextAttributes` | **1** | core/src/types.ts:8 | opentui-core | done |
| `TextBuffer` | **1** | core/src/text-buffer.ts:16 | opentui-core::editor | partial (remaining subset delta: **selection only** — selectionBg/Fg fields exist, paint/measure ignore them. `truncate:true` (applyTruncation + ellipsis/suffix style override, text-buffer-view.zig:1280-1445 + buffer.zig:1908-1952), scissor-gated cells (buffer.zig:1832-1861/2017) and scroll offsets ≠ 0 (viewport x/y slicing + clamped scrollX/scrollY setters, TextBufferRenderable.ts:176-220) are ported; differential text-truncate-on-off / text-scissor-nested / text-scroll-viewport vs 0.5.14 IDENTICAL) |
| `TextBufferRenderable` | 2 | core/src/renderables/TextBufferRenderable.ts:25 | opentui-core::renderables | todo |
| `TextBufferView` | 2 | core/src/text-buffer-view.ts:12 | opentui-core::text_buffer_view | done (contract-level port over the pure-Rust primitives (TextDocument + compute_virtual_lines/apply_truncation) — mechanism≠upstream (no native Zig handle, hence no `ptr`); create/destroy + destroyed-guard, absolute & local selection with Cell/Word/Line expansion, occupancy, wrap width/mode, align, viewport, tab indicator (+color), truncate, lineInfo/logicalLineInfo/getLineSources, get(Selected/Plain)Text, measureForDimensions, getVirtualLineCount; firstLineOffset stored, raster effect deferred) |
| `TextNodeRenderable` | 2 | core/src/renderables/TextNode.ts:34 | opentui-core::text_node | done |
| `TextRenderable` | **1** | core/src/renderables/Text.ts:13 | opentui-core::renderables | done |
| `TextTableRenderable` | 2 | core/src/renderables/TextTable.ts:99 | opentui-core::renderables | done (settled-tree port: options/measure/layout solver + drawGrid borders + per-cell TextBuffer paint; content-diff cell reuse + cell selection deferred — no headless consumer; differential md-table-grid/wide/padding vs 0.5.14 IDENTICAL, allocator unit tests from TextTable.test.ts) |
| `TextareaRenderable` | **1** | core/src/renderables/Textarea.ts:145 | opentui-core::renderables::textarea | done |
| `TimeToFirstDrawRenderable` | 2 | core/src/renderables/TimeToFirstDraw.ts:40 | opentui-core::renderables | done (pulled in by solid `TimeToFirstDraw`; TimeToFirstDraw.test.ts ported + ECMA toFixed) |
| `Timeline` | 2 | core/src/animation/Timeline.ts:284 | opentui-core | todo |
| `TreeSitterClient` | **1** | core/src/lib/tree-sitter/client.ts:74 | opentui-core::tree_sitter | done |
| `VRenderable` | 2 | core/src/renderables/composition/VRenderable.ts:19 | opentui-core::renderables | todo |
| `VignetteEffect` | 2 | core/src/post/effects.ts:268 | opentui-core | todo |
| `Yoga` | 2 | — | opentui-core | done |
| `addDefaultParsers` | **1** | core/src/lib/tree-sitter/client.ts:61 | opentui-core::tree_sitter | done |
| `ansi256IndexToRgb` | 2 | core/src/lib/RGBA.ts:71 | opentui-core::color | done |
| `applyAsciiArt` | 2 | core/src/post/filters.ts:186 | opentui-core | todo |
| `applyBrightness` | 2 | core/src/post/filters.ts:263 | opentui-core | todo |
| `applyChromaticAberration` | 2 | core/src/post/filters.ts:155 | opentui-core | todo |
| `applyGain` | 2 | core/src/post/filters.ts:300 | opentui-core | todo |
| `applyInvert` | 2 | core/src/post/filters.ts:72 | opentui-core | todo |
| `applyNoise` | 2 | core/src/post/filters.ts:103 | opentui-core | todo |
| `applySaturation` | 2 | core/src/post/filters.ts:379 | opentui-core | todo |
| `applyScanlines` | 2 | core/src/post/filters.ts:23 | opentui-core | todo |
| `attributesWithLink` | 2 | core/src/utils.ts:44 | opentui-core::ansi | done |
| `basenameToFiletype` | 2 | core/src/lib/tree-sitter/resolve-ft.ts:102 | opentui-core::tree_sitter | done |
| `bg` | 2 | core/src/lib/styled-text.ts:124 | opentui-core::styled_text::styles | done |
| `bgBlack` | 2 | core/src/lib/styled-text.ts:101 | opentui-core::styled_text::styles | done |
| `bgBlue` | 2 | core/src/lib/styled-text.ts:105 | opentui-core::styled_text::styles | done |
| `bgCyan` | 2 | core/src/lib/styled-text.ts:107 | opentui-core::styled_text::styles | done |
| `bgGreen` | 2 | core/src/lib/styled-text.ts:103 | opentui-core::styled_text::styles | done |
| `bgMagenta` | 2 | core/src/lib/styled-text.ts:106 | opentui-core::styled_text::styles | done |
| `bgRed` | 2 | core/src/lib/styled-text.ts:102 | opentui-core::styled_text::styles | done |
| `bgWhite` | 2 | core/src/lib/styled-text.ts:108 | opentui-core::styled_text::styles | done |
| `bgYellow` | 2 | core/src/lib/styled-text.ts:104 | opentui-core::styled_text::styles | done |
| `black` | 2 | core/src/lib/styled-text.ts:81 | opentui-core::styled_text::styles | done |
| `blink` | 2 | core/src/lib/styled-text.ts:117 | opentui-core::styled_text::styles | done |
| `blue` | 2 | core/src/lib/styled-text.ts:85 | opentui-core::styled_text::styles | done |
| `bold` | 2 | core/src/lib/styled-text.ts:111 | opentui-core::styled_text::styles | done |
| `borderCharsToArray` | 2 | core/src/lib/border.ts:148 | opentui-core | todo |
| `brightBlack` | 2 | core/src/lib/styled-text.ts:91 | opentui-core::styled_text::styles | done |
| `brightBlue` | 2 | core/src/lib/styled-text.ts:95 | opentui-core::styled_text::styles | done |
| `brightCyan` | 2 | core/src/lib/styled-text.ts:97 | opentui-core::styled_text::styles | done |
| `brightGreen` | 2 | core/src/lib/styled-text.ts:93 | opentui-core::styled_text::styles | done |
| `brightMagenta` | 2 | core/src/lib/styled-text.ts:96 | opentui-core::styled_text::styles | done |
| `brightRed` | 2 | core/src/lib/styled-text.ts:92 | opentui-core::styled_text::styles | done |
| `brightWhite` | 2 | core/src/lib/styled-text.ts:98 | opentui-core::styled_text::styles | done |
| `brightYellow` | 2 | core/src/lib/styled-text.ts:94 | opentui-core::styled_text::styles | done |
| `buildKittyKeyboardFlags` | 2 | core/src/renderer.ts:616 | opentui-core::renderer | done |
| `buildTerminalPaletteSignature` | 2 | core/src/lib/terminal-palette.ts:432 | opentui-core | todo |
| `capture` | 2 | core/src/console.ts:65 | opentui-core | todo |
| `clearEnvCache` | 2 | core/src/lib/env.ts:130 | opentui-core | todo |
| `convertGlobalToLocalSelection` | 2 | core/src/lib/selection.ts:167 | opentui-core::renderer | partial |
| `convertThemeToStyles` | 2 | core/src/syntax-style.ts:41 | opentui-core::syntax_style | done |
| `coordinateToCharacterIndex` | 2 | core/src/lib/ascii.font.ts:196 | opentui-core | todo |
| `createCliRenderer` | **1** | core/src/renderer.ts:723 | opentui-core::renderer | done |
| `createClipboard` | **1** | core/src/lib/clipboard.ts:179 | opentui-core | done (sync port: destination policies/remote-gating/pre-abort pinned by clipboard-service.test.ts; async dispose-wait degenerate) |
| `createCoreSlotRegistry` | 2 | core/src/plugins/core-slot.ts:168 | opentui-core | todo |
| `createExtmarksController` | 2 | core/src/lib/extmarks.ts:859 | opentui-core | todo |
| `createHostClipboard` | 2 | core/src/lib/clipboard.ts:165 | opentui-core::clipboard | done (factory :165-166 over the DEFAULT backend; upstream's default is the native Zig clipboard service (host-clipboard.native.ts:342 `createNativeHostClipboardBackend`) — mechanism≠upstream: the port's default is the platform command family (wl-copy/wl-paste → xclip → pbcopy/pbpaste → clip.exe/Get-Clipboard; primary-selection aware, Unsupported where the platform lacks one), output==upstream result statuses; argv family + selection precedence pinned by unit tests, no process spawn in tests; no backend → UnsupportedHostBackend keeps createClipboard's composition contract) |
| `createIcyStreamDemuxer` | 2 | core/src/audio-stream/icy/demuxer.ts:108 | opentui-core | todo |
| `createMarkdownCodeBlockRenderer` | **1** | core/src/renderables/Markdown.ts:171 | opentui-core::renderer | partial |
| `createRendererClipboardAdapter` | **1** | core/src/lib/clipboard.ts:308 | opentui-core::clipboard | done (boundary trait; clipboard-service.test.ts:207-236 ported) |
| `createSlotRegistry` | 2 | core/src/plugins/registry.ts:378 | opentui-core | todo |
| `createTerminalPalette` | 2 | core/src/lib/terminal-palette.ts:395 | opentui-core | todo |
| `createTextAttributes` | 2 | core/src/utils.ts:4 | opentui-core::styled_text | done |
| `createTimeline` | 2 | core/src/animation/Timeline.ts:589 | opentui-core | todo |
| `cyan` | 2 | core/src/lib/styled-text.ts:87 | opentui-core::styled_text::styles | done |
| `decodePasteBytes` | **1** | core/src/lib/paste.ts:12 | opentui-core::paste | done |
| `defaultTextareaKeyBindings` | 2 | core/src/renderables/Textarea.ts:58 | opentui-core::renderables::textarea | done |
| `delegate` | 2 | core/src/renderables/composition/vnode.ts:245 | opentui-core | todo |
| `destroyTreeSitterClient` | 2 | core/src/lib/tree-sitter/index.ts:62 | opentui-core::tree_sitter | done |
| `detectLinks` | 2 | core/src/lib/detect-links.ts:12 | opentui-core::renderables::code | done |
| `dim` | 2 | core/src/lib/styled-text.ts:115 | opentui-core::styled_text::styles | done |
| `engine` | 2 | core/src/animation/Timeline.ts:587 | opentui-core | todo |
| `env` | 2 | core/src/lib/env.ts:193 | opentui-core | todo |
| `envRegistry` | 2 | core/src/lib/env.ts:42 | opentui-core | todo |
| `extToFiletype` | 2 | core/src/lib/tree-sitter/resolve-ft.ts:146 | opentui-core::tree_sitter | done |
| `extensionToFiletype` | 2 | core/src/lib/tree-sitter/resolve-ft.ts:3 | opentui-core::tree_sitter | done |
| `fg` | **1** | core/src/lib/styled-text.ts:120 | opentui-core::styled_text::styles | done |
| `fonts` | 2 | core/src/lib/ascii.font.ts:18 | opentui-core | todo |
| `generateEnvColored` | 2 | core/src/lib/env.ts:164 | opentui-core::color | todo |
| `generateEnvMarkdown` | 2 | core/src/lib/env.ts:134 | opentui-core | todo |
| `getBaseAttributes` | 2 | core/src/types.ts:28 | opentui-core | done |
| `getBorderFromSides` | 2 | core/src/lib/border.ts:125 | opentui-core | todo |
| `getBorderSides` | 2 | core/src/lib/border.ts:134 | opentui-core | todo |
| `getCharacterPositions` | 2 | core/src/lib/ascii.font.ts:155 | opentui-core | todo |
| `getDataPaths` | 2 | core/src/lib/data-paths.ts:107 | opentui-core | todo |
| `getLinkId` | 2 | core/src/utils.ts:50 | opentui-core::ansi | done |
| `getTreeSitterClient` | **1** | core/src/lib/tree-sitter/index.ts:22 | opentui-core::tree_sitter | done |
| `green` | 2 | core/src/lib/styled-text.ts:83 | opentui-core::styled_text::styles | done |
| `h` | 2 | core/src/renderables/composition/vnode.ts:62 | opentui-core | todo |
| `hastToStyledText` | 2 | core/src/lib/hast-styled-text.ts:56 | opentui-core | todo |
| `hexToRgb` | 2 | core/src/lib/RGBA.ts:222 | opentui-core::color | done |
| `hsvToRgb` | 2 | core/src/lib/RGBA.ts:250 | opentui-core::color | done |
| `imageInfo` | 2 | core/src/image.ts:375 | opentui-core | done (empty→TypeError, unknown→unsupported-format, truncated→malformed; PNG IHDR/GIF screen/JPEG SOF/WebP VP8·VP8L·VP8X header geometry real) |
| `infoStringToFiletype` | **1** | core/src/lib/tree-sitter/resolve-ft.ts:173 | opentui-core::tree_sitter | done |
| `instantiate` | 2 | core/src/renderables/composition/vnode.ts:187 | opentui-core | todo |
| `isEditBufferRenderable` | 2 | core/src/renderables/EditBufferRenderable.ts:51 | opentui-core::renderables | todo |
| `isRenderable` | 2 | core/src/Renderable.ts:132 | opentui-core::renderables | done |
| `isStyledText` | **1** | core/src/lib/styled-text.ts:22 | opentui-core::styled_text | done |
| `isTextNodeRenderable` | 2 | core/src/renderables/TextNode.ts:17 | opentui-core::text_node | done |
| `isVNode` | 2 | core/src/renderables/composition/vnode.ts:132 | opentui-core | todo |
| `isValidBorderStyle` | 2 | core/src/lib/border.ts:22 | opentui-core | todo |
| `italic` | 2 | core/src/lib/styled-text.ts:112 | opentui-core::styled_text::styles | done |
| `link` | 2 | core/src/lib/styled-text.ts:129 | opentui-core::styled_text | partial |
| `magenta` | 2 | core/src/lib/styled-text.ts:86 | opentui-core::styled_text::styles | done |
| `maybeMakeRenderable` | 2 | core/src/renderables/composition/vnode.ts:136 | opentui-core::renderables | todo |
| `measureText` | 2 | core/src/lib/ascii.font.ts:105 | opentui-core | todo |
| `nonAlphanumericKeys` | 2 | core/src/lib/parse.keypress.ts:125 | opentui-core::parse_keypress | done |
| `normalizeColorValue` | 2 | core/src/lib/RGBA.ts:217 | opentui-core::color | todo |
| `normalizeIndexedColorIndex` | 2 | core/src/lib/RGBA.ts:63 | opentui-core::color | done |
| `normalizeTerminalPalette` | 2 | core/src/lib/terminal-palette.ts:415 | opentui-core::terminal_palette | done (256-slot palette with ANSI-256 fallback + default fg/bg; `#rgb`/`#rrggbb` hex via `rgba_from_hex`) |
| `parseAlign` | 2 | core/src/lib/yoga.options.ts:44 | opentui-core::layout | todo |
| `parseAlignItems` | 2 | core/src/lib/yoga.options.ts:72 | opentui-core::layout | todo |
| `parseBorderStyle` | 2 | core/src/lib/border.ts:26 | opentui-core | todo |
| `parseBoxSizing` | 2 | core/src/lib/yoga.options.ts:100 | opentui-core::layout | todo |
| `parseColor` | **1** | core/src/lib/RGBA.ts:328 | opentui-core::color | done |
| `parseDimension` | 2 | core/src/lib/yoga.options.ts:114 | opentui-core::layout | todo |
| `parseDirection` | 2 | core/src/lib/yoga.options.ts:128 | opentui-core::layout | todo |
| `parseDisplay` | 2 | core/src/lib/yoga.options.ts:144 | opentui-core::layout | todo |
| `parseEdge` | 2 | core/src/lib/yoga.options.ts:160 | opentui-core::layout | todo |
| `parseFlexDirection` | 2 | core/src/lib/yoga.options.ts:188 | opentui-core::layout | todo |
| `parseGutter` | 2 | core/src/lib/yoga.options.ts:206 | opentui-core::layout | todo |
| `parseJustify` | 2 | core/src/lib/yoga.options.ts:222 | opentui-core::layout | todo |
| `parseKeypress` | 2 | core/src/lib/parse.keypress.ts:214 | opentui-core::parse_keypress | done |
| `parseLogLevel` | 2 | core/src/lib/yoga.options.ts:244 | opentui-core | todo |
| `parseMeasureMode` | 2 | core/src/lib/yoga.options.ts:266 | opentui-core::layout | todo |
| `parseOverflow` | 2 | core/src/lib/yoga.options.ts:282 | opentui-core::layout | todo |
| `parsePositionType` | 2 | core/src/lib/yoga.options.ts:298 | opentui-core::layout | todo |
| `parseUnit` | 2 | core/src/lib/yoga.options.ts:314 | opentui-core::layout | todo |
| `parseWrap` | 2 | core/src/lib/yoga.options.ts:332 | opentui-core::layout | todo |
| `pathToFiletype` | 2 | core/src/lib/tree-sitter/resolve-ft.ts:153 | opentui-core::tree_sitter | done |
| `red` | 2 | core/src/lib/styled-text.ts:82 | opentui-core::styled_text::styles | done |
| `registerCorePlugin` | 2 | core/src/plugins/core-slot.ts:185 | opentui-core | todo |
| `registerEnvVar` | 2 | core/src/lib/env.ts:44 | opentui-core | todo |
| `renderFontToFrameBuffer` | 2 | core/src/lib/ascii.font.ts:220 | opentui-core::buffer | todo |
| `resolveBundledFilePath` | 2 | core/src/platform/runtime.ts:49 | opentui-core | todo |
| `resolveCoreSlot` | 2 | core/src/plugins/core-slot.ts:193 | opentui-core | todo |
| `resolveImageRenderProtocol` | 2 | core/src/renderables/Image.ts:20 | opentui-core | done (the exact Image.ts:20-32 gate ladder incl. sixel-without-resolution → blocks and tmux → blocks; image-renderable.test.ts:342-358 all five assertions pinned) |
| `resolveRenderLib` | 2 | core/src/zig.ts:6938 | opentui-core | todo |
| `reverse` | 2 | core/src/lib/styled-text.ts:116 | opentui-core::styled_text::styles | done |
| `rgbToHex` | 2 | core/src/lib/RGBA.ts:244 | opentui-core | todo |
| `setRenderLibPath` | 2 | core/src/zig.ts:6925 | opentui-core | todo |
| `setupAudio` | 2 | core/src/audio.ts:3685 | opentui-core | done (delegates to Audio.create — the Audio row's narrowing applies; audio.test.ts:244-246 ported) |
| `strikethrough` | 2 | core/src/lib/styled-text.ts:114 | opentui-core::styled_text::styles | done |
| `stringToStyledText` | 2 | core/src/lib/styled-text.ts:36 | opentui-core::styled_text | done |
| `stripAnsiSequences` | 2 | core/src/lib/paste.ts:16 | opentui-core::paste | done |
| `t` | 2 | core/src/lib/styled-text.ts:150 | opentui-core::styled_text | done |
| `terminalNamedSingleStrokeKeys` | 2 | core/src/lib/parse.keypress.ts:127 | opentui-core::parse_keypress | done |
| `treeSitterToStyledText` | 2 | core/src/lib/tree-sitter-styled-text.ts:293 | opentui-core::tree_sitter | done |
| `treeSitterToTextChunks` | 2 | core/src/lib/tree-sitter-styled-text.ts:40 | opentui-core::tree_sitter | done |
| `underline` | 2 | core/src/lib/styled-text.ts:113 | opentui-core::styled_text::styles | done |
| `visualizeRenderableTree` | 2 | core/src/utils.ts:55 | opentui-core::renderables | todo |
| `vstyles` | 2 | core/src/renderables/composition/constructs.ts:89 | opentui-core | todo |
| `white` | 2 | core/src/lib/styled-text.ts:88 | opentui-core::styled_text::styles | done |
| `wrapWithDelegates` | 2 | core/src/renderables/composition/vnode.ts:148 | opentui-core | todo |
| `yellow` | 2 | core/src/lib/styled-text.ts:84 | opentui-core::styled_text::styles | done |

### solid (48 runtime exports)

| symbol | tier | upstream file:line | Rust target | status |
|---|---|---|---|---|
| `BoldSpanRenderable` | 2 | solid/src/elements/catalogue.ts:49 | opentui-solid::element::SpanKind::Bold | done (shape: Tag::Span) |
| `Dynamic` | 2 | solid/src/elements/extras.ts:105 | opentui-solid | deferred (tag-string switch = builder choice) |
| `ItalicSpanRenderable` | 2 | solid/src/elements/catalogue.ts:55 | opentui-solid::element::SpanKind::Italic | done (shape: Tag::Span) |
| `LayoutSlotRenderable` | 2 | solid/src/elements/slot.ts:91 | opentui-solid | partial (marker machinery internalized) |
| `LineBreakRenderable` | 2 | solid/src/elements/catalogue.ts:67 | opentui-solid::element::SpanKind::Break | done (shape: Tag::Span) |
| `LinkRenderable` | 2 | solid/src/elements/catalogue.ts:82 | opentui-solid::element::SpanKind::Link | done (shape: Tag::Span) |
| `Portal` | **1** | solid/src/elements/extras.ts:14 | opentui-solid::Tag::Portal + reconcile mount/unmount | done (solid-portal diff IDENTICAL) |
| `RendererContext` | 2 | solid/src/elements/hooks.ts:12 | opentui-solid::SolidRenderer (is the context) | done |
| `Slot` | 2 | solid/src/plugins/slot.tsx:81 | opentui-solid | deferred (plugin-side slots) |
| `SlotRenderable` | 2 | solid/src/elements/slot.ts:162 | opentui-solid | partial (portal/show markers) |
| `TextSlotRenderable` | 2 | solid/src/elements/slot.ts:54 | opentui-solid | deferred (plugin-side slots) |
| `TimeToFirstDraw` | **1** | solid/src/time-to-first-draw.tsx:15 | opentui-solid::time_to_first_draw | done (extend-registered `time_to_first_draw` intrinsic + builder; registration explicit — Rust has no import side effects) |
| `UnderlineSpanRenderable` | 2 | solid/src/elements/catalogue.ts:61 | opentui-solid::element::SpanKind::Underline | done (shape: Tag::Span) |
| `_render` | 2 | solid-js re-export | SolidRenderer::render_root | done (semantics) |
| `baseComponents` | 2 | solid/src/elements/catalogue.ts:92 | opentui-solid::Tag (ported subset) | partial (box/text/scrollbox/spans) |
| `componentCatalogue` | 2 | solid/src/elements/catalogue.ts:119 | opentui-solid::extend registry | done (thread-local catalogue) |
| `createComponent` | 2 | solid-js re-export | element trees (no component fn indirection) | done (shape: builders) |
| `createDynamic` | 2 | solid/src/elements/extras.ts:75 | opentui-solid | deferred |
| `createElement` | 2 | solid-js re-export | SolidRenderer::mount_element | done (semantics) |
| `createScrollbackWriter` | **1** | solid/src/scrollback.ts:145 | opentui-solid::scrollback | done (4-pass measure + paint_lines; consumed by core `write_to_scrollback` since D1b-core) |
| `createSlot` | 2 | solid/src/plugins/slot.tsx:66 | opentui-solid | deferred (plugin-side slots) |
| `createSlotNode` | 2 | solid/src/reconciler.ts:155 | reconcile marker (Group renderable) | done (shape: marker) |
| `createSolidSlotRegistry` | 2 | solid/src/plugins/slot.tsx:56 | opentui-solid | deferred (plugin-side slots) |
| `createTextNode` | 2 | solid-js re-export | builder::text_node (folds into text content) | done (shape) |
| `effect` | 2 | solid-js re-export | signal subscriptions + flush batch | done (semantics) |
| `extend` | **1** | solid/src/elements/catalogue.ts:133 | opentui-solid::extend::extend | done (solid-extend diff IDENTICAL) |
| `getComponentCatalogue` | 2 | solid/src/elements/catalogue.ts:137 | opentui-solid::extend::get_component_catalogue | done |
| `insert` | 2 | solid-js re-export | reconcile_children (keyed) | done (semantics) |
| `insertNode` | 2 | solid-js re-export | SolidRenderer::mount + insert_child_at | done (semantics) |
| `memo` | 2 | solid-js re-export | reactive::Mapped | done (shape) |
| `mergeProps` | 2 | solid-js re-export | builder prop list (last-wins) | done (shape) |
| `onBlur` | 2 | solid/src/elements/hooks.ts:120 | opentui-solid | todo (focus infra = Input lane) |
| `onFocus` | 2 | solid/src/elements/hooks.ts:108 | opentui-solid | todo (focus infra = Input lane) |
| `onResize` | 2 | solid/src/elements/hooks.ts:24 | SolidRenderer::on_resize | done |
| `render` | **1** | — | opentui-solid::render | done |
| `setProp` | 2 | solid-js re-export | SolidRenderer::set_prop | done |
| `spread` | 2 | solid-js re-export | mount/patch props (per-prop equality skip) | done (semantics) |
| `testRender` | 2 | — | SolidRenderer::new (headless profile) | done |
| `textNodeKeys` | 2 | solid/src/elements/catalogue.ts:31 | element::SpanKind family | done |
| `use` | 2 | solid-js re-export | — (untracked reads by design) | done (shape: no untrack needed) |
| `useKeyHandler` | 2 | solid/src/elements/hooks.ts:106 | keyboard() (renamed useKeyboard upstream) | done |
| `useKeyboard` | **1** | solid/src/elements/hooks.ts:73 | SolidRenderer::keyboard (EventHub + feed_key) | done |
| `usePaste` | **1** | solid/src/elements/hooks.ts:91 | SolidRenderer::paste (EventHub + feed_paste) | done |
| `useRenderer` | **1** | solid/src/elements/hooks.ts:14 | SolidRenderer::use_renderer | done |
| `useSelectionHandler` | 2 | solid/src/elements/hooks.ts:132 | opentui-solid | todo (selection = EditBuffer lane) |
| `useTerminalDimensions` | **1** | solid/src/elements/hooks.ts:36 | SolidRenderer::terminal_dimensions (signal) | done |
| `useTimeline` | 2 | solid/src/elements/hooks.ts:144 | opentui-core (timeline = render lane target) | deferred |
| `writeSolidToScrollback` | 2 | solid/src/scrollback.ts:221 | opentui-solid::scrollback::write_solid_to_scrollback | done (convenience wired through `CliRenderer::write_to_scrollback` since D1b-surface; the writer shape landed D1b-core) |

### keymap (4 runtime exports)

| symbol | tier | upstream file:line | Rust target | status |
|---|---|---|---|---|
| `KEYMAP_EXTENSION_CONTEXT` | 2 | keymap/src/services/extension-context.ts:7 | opentui-keymap | done |
| `Keymap` | 2 | keymap/src/keymap.ts:73 | opentui-keymap | done |
| `stringifyKeySequence` | 2 | keymap/src/services/keys.ts:132 | opentui-keymap | done |
| `stringifyKeyStroke` | **1** | keymap/src/services/keys.ts:120 | opentui-keymap | done |

### keymap/opentui (3 runtime exports)

| symbol | tier | upstream file:line | Rust target | status |
|---|---|---|---|---|
| `createDefaultOpenTuiKeymap` | **1** | keymap/src/opentui.ts:126 | opentui-keymap | done |
| `createOpenTuiKeymap` | 2 | keymap/src/opentui.ts:118 | opentui-keymap | done |
| `createOpenTuiKeymapHost` | 2 | keymap/src/opentui.ts:53 | opentui-keymap | done |

### keymap/solid (5 runtime exports)

| symbol | tier | upstream file:line | Rust target | status |
|---|---|---|---|---|
| `KeymapProvider` | **1** | keymap/src/react/index.ts:34 | opentui-keymap | done |
| `reactiveMatcherFromSignal` | 2 | keymap/src/solid/index.ts:156 | opentui-keymap | done |
| `useBindings` | 2 | keymap/src/react/index.ts:102 | opentui-keymap | done |
| `useKeymap` | 2 | keymap/src/react/index.ts:54 | opentui-keymap | done |
| `useKeymapSelector` | **1** | keymap/src/solid/index.ts:85 | opentui-keymap | done |

### markdown + code lane (O2g) additions (beyond the frozen 339)

Ported by lane `lane/md` — the marked@17.0.1 lexer (the exact
dependency of @opentui/core 0.5.14, pinned by the frozen token corpus
`crates/opentui-core/tests/data/marked-corpus.json`) and the oracle
highlight corpus
(`crates/opentui-core/tests/data/highlight-corpus.json`, generated by
`tools/diff/js/highlight-corpus.ts` from the real package's
TreeSitterClient). Not counted in the 339.

| symbol | upstream file:line | Rust target | status |
|---|---|---|---|
| `Lexer.lex` (marked) | node_modules/marked@17.0.1 (dependency) | opentui-core::renderables::markdown_parser | done (corpus-pinned) |
| `parseMarkdownIncremental` | renderables/markdown-parser.ts:13 | opentui-core::renderables::markdown_parser | done (settled-content slice) |
| `detectLinks` | lib/detect-links.ts:12 | opentui-core::renderables::code | done |
| `addMarkdownLinkHighlights` | renderables/Markdown.ts:724 | opentui-core::renderables::code | done |
| grammar vendoring (5 langs) | lib/tree-sitter/parsers-config.ts | third_party/tree-sitter-grammars + build.rs | done (byte-parity) |

### keymap lane O2c additions (beyond the frozen 339; v2-integration surface)

Ported by lane `lane/keymap` — the integrations v2's
`packages/tui/src/context/keymap.tsx:78-146` + `config/keybind.ts`
consume. Not counted in the 339.

| symbol | upstream file:line | Rust target | status |
|---|---|---|---|
| `createBindingLookup` | keymap/src/extras/binding-lookup.ts:242 | opentui-keymap::extras | done |
| `formatCommandBindings` | keymap/src/extras/formatting.ts:111 | opentui-keymap::extras | done |
| `formatKeySequence` | keymap/src/extras/formatting.ts:95 | opentui-keymap::extras | done |
| `commandBindings` | keymap/src/extras/command-bindings.ts:28 | opentui-keymap::extras | done |
| `registerTimedLeader` | keymap/src/addons/universal/timed-leader.ts:14 | opentui-keymap::addons | done |
| `registerEscapeClearsPendingSequence` | keymap/src/addons/universal/escape-clears-pending-sequence.ts:16 | opentui-keymap::addons | done |
| `registerBackspacePopsPendingSequence` | keymap/src/addons/universal/backspace-pops-pending-sequence.ts:15 | opentui-keymap::addons | done |
| `registerCommaBindings` | keymap/src/addons/universal/comma-bindings.ts:34 | opentui-keymap::addons | done |
| `registerBaseLayoutFallback` | keymap/src/addons/opentui/base-layout.ts:28 | opentui-keymap::addons | done |
| `registerManagedTextareaLayer` | keymap/src/addons/opentui/edit-buffer-bindings.ts:548 | opentui-keymap::textarea | done |
| `registerEditBufferCommands` | keymap/src/addons/opentui/edit-buffer-bindings.ts:526 | opentui-keymap::textarea | done |
| `registerLeader` | keymap/src/addons/universal/leader.ts:37 | opentui-keymap::addons | done |
| `defaultBindingParser` | keymap/src/addons/universal/default-parser.ts:400 | opentui-keymap::parser | done |
| `registerDefaultKeys` | keymap/src/addons/universal/default-parser.ts:529 | opentui-keymap::addons | done |

### lane D1a additions (beyond the frozen 339; renderer lifecycle config surface)

Ported by lane `lane/d1a` (tui-full-convergence D1a — CliRenderer
runtime lifecycle + config face). Not runtime exports upstream
(type-only / interface members of `CliRendererConfig`), so they ride
this supplementary table like the O2g/O2c additions above.

| symbol | upstream file:line | Rust target | status |
|---|---|---|---|
| `CliRendererConfig.stdin` | core/src/renderer.ts:126 | opentui-core::renderer::StdinSource | done (intent enum — the live stream is the Phase A `Terminal` session's TtyDevice; upstream duck-types a `Readable`) |
| `CliRendererConfig.exitOnCtrlC` | core/src/renderer.ts:152 | opentui-core::renderer::CliRendererConfig | done (dispatch_key arm, renderer.ts:1252-1257; sync destroy — no microtask queue) |
| `CliRendererConfig.clearOnShutdown` | core/src/renderer.ts:158 | opentui-core::renderer::CliRendererConfig | partial (recorded; teardown is RAII-unconditional in the port — restoring the host terminal is not optional) |
| `CliRendererConfig.gatherStats` | core/src/renderer.ts:183 | opentui-core::renderer::CliRendererConfig | done (flag + setGatherStats, renderer.ts:4924; the stats/overlay consumer is deferred) |
| `CliRendererConfig.consoleMode` | core/src/renderer.ts:222 | opentui-core::renderer::ConsoleMode | partial ("disabled" semantics implemented, renderer.ts:1625-1632; "console-overlay" arm records intent — overlay renderable deferred) |
| `CliRendererConfig.consoleOptions` | core/src/renderer.ts:203 | opentui-core::renderer::ConsoleOptions | partial (the `keyBindings` slice only, console.ts:277; styling knobs ride the overlay family) |
| `ConsoleAction` | core/src/console.ts:231 | opentui-core::renderer::ConsoleAction | done (enum parity) |
| `ConsoleKeyBinding` | core/src/console.ts:243 | opentui_core::renderer::ConsoleKeyBinding | done (flattened `BaseKeyBinding<ConsoleAction>`) |
| `EventEmitter.setMaxListeners` (renderer face) | node:events (inherited, renderer.ts:57) | opentui-core::renderer::CliRenderer::set_max_listeners | partial (cap stored + getter; Node's warning channel needs a logger seam — deferred) |

### lane D1b-core additions (beyond the frozen 339; split-footer scrollback runtime base)

Ported by lane `lane/d1b-core` (tui-full-convergence D1b-core — the
split-footer scrollback RUNTIME base: the write path, the
`ExternalOutputCommit` FIFO, tail-column newline ownership, and the
flush adjudication; the SURFACE family — `createScrollbackSurface`,
`screenMode`/`footerHeight`/`externalOutputMode` config enums —
landed with lane D1b-surface; see its additions table below).
Type/interface members, not runtime
exports, so they ride this supplementary table like the D1a
additions above.

| symbol | upstream file:line | Rust target | status |
|---|---|---|---|
| `ScrollbackRenderContext` | core/src/renderer.ts:295 | opentui-core::scrollback::ScrollbackRenderContext | done (width/widthMethod/tailColumn; widthMethod defaults until surface wiring) |
| `ScrollbackSnapshot` | core/src/renderer.ts:304 | opentui-core::scrollback::ScrollbackSnapshot | done (painted-rows payload — the opentui-solid `createScrollbackWriter` consumption contract; `teardown` is the scratch renderer's drop) |
| `ScrollbackWriter` | core/src/renderer.ts:314 | opentui-core::scrollback::ScrollbackWriter | done (`FnOnce(&ctx) -> ScrollbackSnapshot`) |
| `CliRenderer.writeToScrollback` | core/src/renderer.ts:2363 | opentui-core::renderer::CliRenderer::write_to_scrollback | done (mode gate returns `ScrollbackWriteError` instead of throwing; clamps per :2477-2499) |
| `ExternalOutputCommit` | core/src/renderer.ts:395 | opentui-core::scrollback::ExternalOutputCommit | done (payload = painted rows; no native OptimizedBuffer — bounded divergence) |
| `ExternalOutputQueue` | core/src/renderer.ts:403 | opentui-core::scrollback::ExternalOutputQueue | done (writeSnapshot/peek/claim/drop/clear, :405-441) |
| `CliRendererExternalOutputEvent` | core/src/renderer.ts:252 | opentui-core::scrollback::CliRendererExternalOutputEvent | done (= the commit; emitted only with listeners, :2610) |
| `CliRenderEvents.EXTERNAL_OUTPUT` | core/src/renderer.ts:755 | opentui-core::renderer::CliRenderEvent::ExternalOutput | done (enum parity — the row-221 partial note updated) |
| `makeRoomForRendererOutput` (setup arm) | native/src/ansi.zig:418 (renderer.zig:566-569) | opentui-core::terminal::modes::make_room_for_renderer_output + SetupOptions::use_alt_screen/reserve_surface_height | done (non-alt reservation arm; default alt-screen unchanged) |
| flush adjudication family (`flushPendingSplitCommits` / `flushPendingSplitOutputBeforeTransition` / tail-column math) | core/src/renderer.ts:2497-2571, 2729-2826, 2894-2918 | opentui-core::scrollback + CliRenderer::flush_pending_split_commits / flush_pending_split_output_before_transition / get_pending_split_tail_column | done (bounded 8/tick drain, FIFO drop, re-request; `Backpressured` arm unreachable without the native gate — parity shape only) |
| `resetSplitFooterForReplay` | core/src/renderer.ts:2442 | opentui-core::renderer::CliRenderer::reset_split_footer_for_replay | done (D1b-surface; flush→clear→tail reset + the :2466-2473 control burst; headless counts as an active terminal — bounded divergence) |
| `createScrollbackSurface` | core/src/renderer.ts:1984 | opentui-core::renderer::CliRenderer::create_scrollback_surface | done (D1b-surface; see the surface-family rows below) |
| `writeSolidToScrollback` (core consumer wiring) | solid/src/scrollback.ts:221 | opentui-solid::scrollback::write_solid_to_scrollback | done (D1b-surface convenience over `write_to_scrollback`; the row-~530 shape note superseded) |

### lane D1b-surface additions (split-footer surface family — closes gap G-211)

Ported by lane `lane/d1b-surface` (tui-full-convergence phase-D1b third
lane; the config/mode half and the retained-surface half of the
split-footer family, over the D1b-core runtime base). Type/interface
members riding the supplementary tables like the D1b-core rows above.

| symbol | upstream file:line | Rust target | status |
|---|---|---|---|
| `ScreenMode` | core/src/renderer.ts:242 | opentui-core::renderer::ScreenMode | done (enum parity; default "alternate-screen" :370) |
| `ExternalOutputMode` | core/src/renderer.ts:250 | opentui-core::renderer::ExternalOutputMode | done (enum parity; default "passthrough" :379) |
| `CliRendererConfig.screenMode` | core/src/renderer.ts:202 | CliRendererConfig::screen_mode | done |
| `CliRendererConfig.footerHeight` | core/src/renderer.ts:205 | CliRendererConfig::footer_height | done (Option<u32> = upstream's undefined; DEFAULT_FOOTER_HEIGHT=12 :341) |
| `CliRendererConfig.externalOutputMode` | core/src/renderer.ts:208 | CliRendererConfig::external_output_mode | done (Option; defaulted per screenMode in resolve_modes) |
| `normalizeFooterHeight` / `DEFAULT_FOOTER_HEIGHT` / `MAX_SCROLLBACK_SURFACE_HEIGHT_PASSES` | core/src/renderer.ts:341-342, 348-362 | opentui-core::renderer::normalize_footer_height / consts | done (non-finite arm structurally unreachable for u32) |
| `resolveModes` (+ OTUI_USE_ALTERNATE_SCREEN/OTUI_OVERRIDE_STDOUT overrides) | core/src/renderer.ts:365-393 | opentui-core::renderer::resolve_modes -> ResolvedModes/RendererModeError | done (decision-table tested; constructor arms the capture runtime :1074/:1291 — panics where upstream throws, the Result seam is pub) |
| `renderer.widthMethod` (caps wiring — D1b-core divergence ④ closed) | core/src/renderer.ts:1468 | CliRenderer::width_method + set_capabilities | done (caps?.unicode ?? zig default) |
| `enqueueRenderedScrollbackCommit` | core/src/renderer.ts:2583-2614 | CliRenderer::enqueue_rendered_scrollback_commit | done (shared by writeToScrollback + surface commitRows) |
| `ScrollbackSurfaceOptions` / `ScrollbackSurfaceCommitOptions` | core/src/renderer.ts:316-323 | opentui-core::renderer::ScrollbackSurfaceOptions / ScrollbackSurfaceCommitOptions | done |
| `ScrollbackSurface` (render/settle/commitRows/destroy + assert ladder) | core/src/renderer.ts:325-338, 2060-2306 | opentui-core::renderer::ScrollbackSurface | done (scratch-renderer shape — core renderables bind to their taffy tree, commit_rows takes the parent renderer explicitly; settle is sync — highlight pipeline sync; bounded divergences) |
| `ScrollbackSnapshotRenderContext` / `claimFirstLineOffset` | core/src/renderer.ts:1994-2013 | ScrollbackSurface::render_context (the scratch renderer IS the context face) | done (port shape; first-line splice owned by the tail-column math) |
| `resetSplitFooterForReplay` | core/src/renderer.ts:2442-2475 | CliRenderer::reset_split_footer_for_replay | done (clearSavedLines empties the transcript model; control burst live-only) |
| `CliRendererExternalOutputEvent` peek access | — | CliRenderer::external_output_queue | done (read-only peek for host assertions) |

### lane D1c additions (six small API gaps — Gate D0 approved)

Ported by lane `lane/d1c` (tui-full-convergence phase-D1 fourth lane).
Members ride this supplementary table like the D1b lanes above; the
frozen rows touched by the lane (`TextBufferView`, `createHostClipboard`,
`TerminalPalette`, `normalizeTerminalPalette`) were updated in place.

| symbol | upstream file:line | Rust target | status |
|---|---|---|---|
| `OnHighlightCallback` / `HighlightContext` | core/src/renderables/Code.ts:11-21 | opentui-core::renderables::code | done (sync-settled port: upstream's async `Promise` collapses — the port highlights synchronously at construction and the differential oracle captures the settled frame; invoked on the raw highlights before the markdown built-in hook) |
| `OnChunksCallback` / `ChunkRenderContext` | core/src/renderables/Code.ts:23-32 | opentui-core::renderables::code | done (same sync ruling; runs after `treeSitterToTextChunks`, before `detectLinks`) |
| `CodePipelineHooks` | — (port-side carrier for Code.ts:42-43/67) | opentui-core::renderables::code | done (`build_code_document_with_hooks`; default hooks are no-ops — 119 differential scenarios stay IDENTICAL) |
| `EditorTraits` / `EditorCapture` / `sameTraits` | core/src/renderables/EditBufferRenderable.ts:28-49 | opentui-core::editor | done (ref's `prompt/traits.ts:1` consumes the type — the upstream truth is EditBufferRenderable, not ref-local; `TextareaState::set_traits` is the `traits` setter :283-291 — no-op suppressed via `sameTraits`, suspend syncs the keymap-suspension projection; TRAITS_CHANGED's observable is the returned bool) |
| `TerminalColors` / `NormalizedTerminalPalette` / hex parse | core/src/lib/terminal-palette.ts:24-41 | opentui-core::terminal_palette | done (crate-root module — the `terminal` module is feature-gated and this is a pure data contract the ref theme layer consumes unconditionally; ref sites: context/theme.tsx:1,381 + mini/theme.ts:1,273,297) |
| `HostCommandBackend` / `CommandHostClipboardBackend` / `HostCommandSpec` | — (port-side mechanism behind createHostClipboard) | opentui-core::clipboard | done (see the `createHostClipboard` frozen row) |
| `ensureRuntimePluginSupport` (+`SolidRuntimePluginSupportOptions`, rewrite options) | solid/scripts/runtime-plugin-support-configure.ts:100-125 (via package.json:39-42 `./runtime-plugin-support/configure`) | opentui-solid::runtime_plugin_support | done (contract-level per Gate D0: mechanism≠upstream — no bun loader plugin / solid transform, a process-global install record instead; output==upstream — first install `true`, idempotent `false`, the three compat errors word-for-word (:75-97), defaultRuntimeModules :52-61 + `additional` form the installed specifier set) |
