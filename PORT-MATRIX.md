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
| solid glue | consumer-side: opencode-rust screens mount via builders (their PTY goldens) |

## Stats (bootstrap snapshot)

| package | exports | tier 1 | done | partial | todo |
|---|---|---|---|---|---|
| @opentui/core | 279 | 55 | 14 | 4 | 261 |
| @opentui/solid | 48 | 10 | 5 | 4 | 39 |
| @opentui/keymap (+subpaths) | 12 | 14* | 4 | 2 | 6 |
| **total** | **339** | **79** | **23** | **10** | **306** |

\* keymap subpath symbols land in the same Rust crate; counted once in
the 79 tier-1 set.

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
| `ArrowRenderable` | 2 | core/src/renderables/ScrollBar.ts:320 | opentui-core::renderables | todo |
| `Audio` | **1** | core/src/audio.ts:2752 | opentui-core | todo |
| `AudioCaptureStream` | 2 | core/src/audio.ts:1759 | opentui-core | todo |
| `AudioCaptureStreamError` | 2 | core/src/audio.ts:371 | opentui-core | todo |
| `AudioInitializationError` | 2 | core/src/audio.ts:358 | opentui-core | todo |
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
| `CliRenderEvents` | **1** | core/src/renderer.ts:750 | opentui-core::renderer | done |
| `CliRenderer` | **1** | core/src/renderer.ts:780 | opentui-core::renderer | done |
| `Clipboard` | 2 | core/src/lib/clipboard.ts:337 | opentui-core | todo |
| `ClipboardTarget` | 2 | core/src/lib/clipboard.ts:292 | opentui-core | todo |
| `CloudsEffect` | 2 | core/src/post/effects.ts:494 | opentui-core | todo |
| `Code` | 2 | core/src/renderables/composition/constructs.ts:60 | opentui-core | todo |
| `CodeRenderable` | **1** | core/src/renderables/Code.ts:48 | opentui-core::renderables | todo |
| `ConsolePosition` | 2 | core/src/console.ts:217 | opentui-core | todo |
| `DEFAULT_BACKGROUND_RGB` | 2 | core/src/lib/RGBA.ts:6 | opentui-core | done |
| `DEFAULT_FOREGROUND_RGB` | 2 | core/src/lib/RGBA.ts:5 | opentui-core | done |
| `DEUTERANOPIA_COMP_MATRIX` | 2 | core/src/post/matrices.ts:126 | opentui-core | todo |
| `DEUTERANOPIA_SIM_MATRIX` | 2 | core/src/post/matrices.ts:46 | opentui-core | todo |
| `DataPathsManager` | 2 | core/src/lib/data-paths.ts:34 | opentui-core | todo |
| `DebugOverlayCorner` | 2 | core/src/types.ts:92 | opentui-core | todo |
| `DiffRenderable` | **1** | core/src/renderables/Diff.ts:55 | opentui-core::renderables | todo |
| `DistortionEffect` | 2 | core/src/post/effects.ts:29 | opentui-core | todo |
| `EditBuffer` | 2 | core/src/edit-buffer.ts:19 | opentui-core::buffer | todo |
| `EditBufferRenderable` | 2 | core/src/renderables/EditBufferRenderable.ts:85 | opentui-core::renderables | todo |
| `EditBufferRenderableEvents` | 2 | core/src/renderables/EditBufferRenderable.ts:36 | opentui-core::renderables | todo |
| `EditorView` | 2 | core/src/editor-view.ts:23 | opentui-core | todo |
| `EmbeddedTerminalRenderable` | **1** | core/src/renderables/EmbeddedTerminal.ts:42 | opentui-core::renderables | todo |
| `ExtmarksController` | 2 | core/src/lib/extmarks.ts:33 | opentui-core | todo |
| `FlamesEffect` | 2 | core/src/post/effects.ts:605 | opentui-core | todo |
| `FrameBuffer` | 2 | core/src/renderables/composition/constructs.ts:56 | opentui-core::buffer | todo |
| `FrameBufferRenderable` | 2 | core/src/renderables/FrameBuffer.ts:11 | opentui-core::renderables | todo |
| `GRAYSCALE_MATRIX` | 2 | core/src/post/matrices.ts:251 | opentui-core | todo |
| `GREENSCALE_MATRIX` | 2 | core/src/post/matrices.ts:231 | opentui-core | todo |
| `Generic` | 2 | core/src/renderables/composition/constructs.ts:28 | opentui-core | todo |
| `INVERT_MATRIX` | 2 | core/src/post/matrices.ts:271 | opentui-core | todo |
| `ImageError` | 2 | core/src/image.ts:173 | opentui-core | todo |
| `ImageLoadError` | 2 | core/src/image.ts:16 | opentui-core | todo |
| `ImageRenderable` | 2 | core/src/renderables/Image.ts:41 | opentui-core::renderables | todo |
| `Input` | 2 | core/src/renderables/composition/constructs.ts:44 | opentui-core | todo |
| `InputRenderable` | **1** | core/src/renderables/Input.ts:46 | opentui-core::renderables | todo |
| `InputRenderableEvents` | 2 | core/src/renderables/Input.ts:29 | opentui-core::renderables | todo |
| `InternalKeyHandler` | 2 | core/src/lib/KeyHandler.ts:134 | opentui-core::renderer | todo |
| `KeyEvent` | **1** | core/src/lib/KeyHandler.ts:5 | opentui-core::renderer | partial |
| `KeyHandler` | 2 | core/src/lib/KeyHandler.ts:99 | opentui-core::renderer | todo |
| `LayoutEvents` | 2 | core/src/Renderable.ts:39 | opentui-core | todo |
| `LineNumberRenderable` | **1** | core/src/renderables/LineNumberRenderable.ts:365 | opentui-core::renderables | todo |
| `LinearScrollAccel` | 2 | core/src/lib/scroll-acceleration.ts:6 | opentui-core | todo |
| `LogLevel` | 2 | core/src/zig.ts:2339 | opentui-core | todo |
| `MAX_LINK_URL_BYTES` | 2 | core/src/zig.ts:105 | opentui-core::ansi | todo |
| `MacOSScrollAccel` | **1** | core/src/lib/scroll-acceleration.ts:34 | opentui-core | todo |
| `MarkdownRenderable` | **1** | core/src/renderables/Markdown.ts:270 | opentui-core::renderables | todo |
| `MouseButton` | **1** | core/src/renderer.ts:705 | opentui-core::renderer | partial |
| `MouseEvent` | **1** | core/src/renderer.ts:653 | opentui-core::renderer | partial |
| `MouseParser` | 2 | core/src/lib/parse.mouse.ts:22 | opentui-core::renderer | todo |
| `NativeAudioStreamCloseReason` | 2 | core/src/zig.ts:97 | opentui-core | todo |
| `NativeAudioStreamFormat` | 2 | core/src/zig.ts:99 | opentui-core | todo |
| `NativeAudioStreamState` | 2 | core/src/zig.ts:95 | opentui-core | todo |
| `NativeClipboardCancelStatus` | 2 | core/src/zig.ts:161 | opentui-core | todo |
| `NativeClipboardCopyStatus` | 2 | core/src/zig.ts:167 | opentui-core | todo |
| `NativeClipboardDestroyStatus` | 2 | core/src/zig.ts:175 | opentui-core | todo |
| `NativeClipboardOperationStatus` | 2 | core/src/zig.ts:138 | opentui-core | todo |
| `NativeClipboardShutdownStatus` | 2 | core/src/zig.ts:181 | opentui-core | todo |
| `NativeClipboardStartStatus` | 2 | core/src/zig.ts:152 | opentui-core | todo |
| `NativeImage` | 2 | core/src/image.ts:383 | opentui-core | todo |
| `NativeImagePool` | 2 | core/src/image.ts:659 | opentui-core | todo |
| `NativeMeasureTargetKind` | 2 | core/src/zig.ts:2414 | opentui-core | todo |
| `NativeSpanFeed` | 2 | core/src/NativeSpanFeed.ts:34 | opentui-core::buffer | todo |
| `OptimizedBuffer` | **1** | core/src/buffer.ts:54 | opentui-core::buffer | done |
| `PROTANOPIA_COMP_MATRIX` | 2 | core/src/post/matrices.ts:106 | opentui-core | todo |
| `PROTANOPIA_SIM_MATRIX` | 2 | core/src/post/matrices.ts:26 | opentui-core | todo |
| `PasteEvent` | **1** | core/src/lib/KeyHandler.ts:64 | opentui-core::renderer | partial |
| `RGBA` | **1** | core/src/lib/RGBA.ts:90 | opentui-core::color | done |
| `RainbowTextEffect` | 2 | core/src/post/effects.ts:840 | opentui-core | todo |
| `Renderable` | **1** | core/src/Renderable.ts:225 | opentui-core::renderables | done |
| `RenderableEvents` | 2 | core/src/Renderable.ts:44 | opentui-core::renderables | todo |
| `RendererControlState` | 2 | core/src/renderer.ts:769 | opentui-core::renderer | todo |
| `RootRenderable` | 2 | core/src/Renderable.ts:1743 | opentui-core::renderables | done |
| `RootTextNodeRenderable` | **1** | core/src/renderables/TextNode.ts:363 | opentui-core::renderables | todo |
| `SEPIA_MATRIX` | 2 | core/src/post/matrices.ts:2 | opentui-core | todo |
| `SOLARIZATION_MATRIX` | 2 | core/src/post/matrices.ts:191 | opentui-core | todo |
| `SYNTHWAVE_MATRIX` | 2 | core/src/post/matrices.ts:211 | opentui-core | todo |
| `ScrollBarRenderable` | 2 | core/src/renderables/ScrollBar.ts:20 | opentui-core::renderables | todo |
| `ScrollBox` | 2 | core/src/renderables/composition/constructs.ts:64 | opentui-core | todo |
| `ScrollBoxRenderable` | **1** | core/src/renderables/ScrollBox.ts:109 | opentui-core::renderables | todo |
| `Select` | 2 | core/src/renderables/composition/constructs.ts:48 | opentui-core | todo |
| `SelectRenderable` | 2 | core/src/renderables/Select.ts:66 | opentui-core::renderables | todo |
| `SelectRenderableEvents` | 2 | core/src/renderables/Select.ts:61 | opentui-core::renderables | todo |
| `Selection` | 2 | core/src/lib/selection.ts:27 | opentui-core | todo |
| `SliderRenderable` | 2 | core/src/renderables/Slider.ts:20 | opentui-core::renderables | todo |
| `SlotRegistry` | 2 | core/src/plugins/registry.ts:40 | opentui-core | todo |
| `SlotRenderable` | 2 | core/src/plugins/core-slot.ts:241 | opentui-core::renderables | todo |
| `StdinParser` | 2 | core/src/lib/stdin-parser.ts:587 | opentui-core | todo |
| `StyledText` | **1** | core/src/lib/styled-text.ts:26 | opentui-core | partial |
| `SyntaxStyle` | **1** | core/src/syntax-style.ts:76 | opentui-core | todo |
| `SystemClock` | 2 | core/src/lib/clock.ts:11 | opentui-core | todo |
| `TECHNICOLOR_MATRIX` | 2 | core/src/post/matrices.ts:170 | opentui-core | todo |
| `TRITANOPIA_COMP_MATRIX` | 2 | core/src/post/matrices.ts:146 | opentui-core | todo |
| `TRITANOPIA_SIM_MATRIX` | 2 | core/src/post/matrices.ts:66 | opentui-core | todo |
| `TabSelect` | 2 | core/src/renderables/composition/constructs.ts:52 | opentui-core | todo |
| `TabSelectRenderable` | 2 | core/src/renderables/TabSelect.ts:74 | opentui-core::renderables | todo |
| `TabSelectRenderableEvents` | 2 | core/src/renderables/TabSelect.ts:55 | opentui-core::renderables | todo |
| `TargetChannel` | **1** | core/src/types.ts:99 | opentui-core | partial |
| `TerminalConsole` | 2 | core/src/console.ts:322 | opentui-core | todo |
| `TerminalPalette` | 2 | core/src/lib/terminal-palette.ts:93 | opentui-core | todo |
| `Text` | 2 | core/src/renderables/composition/constructs.ts:36 | opentui-core | done |
| `TextAttributes` | **1** | core/src/types.ts:8 | opentui-core | done |
| `TextBuffer` | **1** | core/src/text-buffer.ts:16 | opentui-core | todo |
| `TextBufferRenderable` | 2 | core/src/renderables/TextBufferRenderable.ts:25 | opentui-core::renderables | todo |
| `TextBufferView` | 2 | core/src/text-buffer-view.ts:12 | opentui-core | todo |
| `TextNodeRenderable` | 2 | core/src/renderables/TextNode.ts:34 | opentui-core::renderables | todo |
| `TextRenderable` | **1** | core/src/renderables/Text.ts:13 | opentui-core::renderables | done |
| `TextTableRenderable` | 2 | core/src/renderables/TextTable.ts:99 | opentui-core::renderables | todo |
| `TextareaRenderable` | **1** | core/src/renderables/Textarea.ts:145 | opentui-core::renderables | todo |
| `TimeToFirstDrawRenderable` | 2 | core/src/renderables/TimeToFirstDraw.ts:40 | opentui-core::renderables | todo |
| `Timeline` | 2 | core/src/animation/Timeline.ts:284 | opentui-core | todo |
| `TreeSitterClient` | **1** | core/src/lib/tree-sitter/client.ts:74 | opentui-core | todo |
| `VRenderable` | 2 | core/src/renderables/composition/VRenderable.ts:19 | opentui-core::renderables | todo |
| `VignetteEffect` | 2 | core/src/post/effects.ts:268 | opentui-core | todo |
| `Yoga` | 2 | — | opentui-core | done |
| `addDefaultParsers` | **1** | core/src/lib/tree-sitter/client.ts:61 | opentui-core | todo |
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
| `basenameToFiletype` | 2 | core/src/lib/tree-sitter/resolve-ft.ts:102 | opentui-core | todo |
| `bg` | 2 | core/src/lib/styled-text.ts:124 | opentui-core::color | todo |
| `bgBlack` | 2 | core/src/lib/styled-text.ts:101 | opentui-core::color | todo |
| `bgBlue` | 2 | core/src/lib/styled-text.ts:105 | opentui-core::color | todo |
| `bgCyan` | 2 | core/src/lib/styled-text.ts:107 | opentui-core::color | todo |
| `bgGreen` | 2 | core/src/lib/styled-text.ts:103 | opentui-core::color | todo |
| `bgMagenta` | 2 | core/src/lib/styled-text.ts:106 | opentui-core::color | todo |
| `bgRed` | 2 | core/src/lib/styled-text.ts:102 | opentui-core::color | todo |
| `bgWhite` | 2 | core/src/lib/styled-text.ts:108 | opentui-core::color | todo |
| `bgYellow` | 2 | core/src/lib/styled-text.ts:104 | opentui-core::color | todo |
| `black` | 2 | core/src/lib/styled-text.ts:81 | opentui-core | todo |
| `blink` | 2 | core/src/lib/styled-text.ts:117 | opentui-core::ansi | todo |
| `blue` | 2 | core/src/lib/styled-text.ts:85 | opentui-core | todo |
| `bold` | 2 | core/src/lib/styled-text.ts:111 | opentui-core::ansi | todo |
| `borderCharsToArray` | 2 | core/src/lib/border.ts:148 | opentui-core | todo |
| `brightBlack` | 2 | core/src/lib/styled-text.ts:91 | opentui-core | todo |
| `brightBlue` | 2 | core/src/lib/styled-text.ts:95 | opentui-core | todo |
| `brightCyan` | 2 | core/src/lib/styled-text.ts:97 | opentui-core | todo |
| `brightGreen` | 2 | core/src/lib/styled-text.ts:93 | opentui-core | todo |
| `brightMagenta` | 2 | core/src/lib/styled-text.ts:96 | opentui-core | todo |
| `brightRed` | 2 | core/src/lib/styled-text.ts:92 | opentui-core | todo |
| `brightWhite` | 2 | core/src/lib/styled-text.ts:98 | opentui-core | todo |
| `brightYellow` | 2 | core/src/lib/styled-text.ts:94 | opentui-core | todo |
| `buildKittyKeyboardFlags` | 2 | core/src/renderer.ts:616 | opentui-core::renderer | todo |
| `buildTerminalPaletteSignature` | 2 | core/src/lib/terminal-palette.ts:432 | opentui-core | todo |
| `capture` | 2 | core/src/console.ts:65 | opentui-core | todo |
| `clearEnvCache` | 2 | core/src/lib/env.ts:130 | opentui-core | todo |
| `convertGlobalToLocalSelection` | 2 | core/src/lib/selection.ts:167 | opentui-core | todo |
| `convertThemeToStyles` | 2 | core/src/syntax-style.ts:41 | opentui-core | todo |
| `coordinateToCharacterIndex` | 2 | core/src/lib/ascii.font.ts:196 | opentui-core | todo |
| `createCliRenderer` | **1** | core/src/renderer.ts:723 | opentui-core::renderer | done |
| `createClipboard` | **1** | core/src/lib/clipboard.ts:179 | opentui-core | todo |
| `createCoreSlotRegistry` | 2 | core/src/plugins/core-slot.ts:168 | opentui-core | todo |
| `createExtmarksController` | 2 | core/src/lib/extmarks.ts:859 | opentui-core | todo |
| `createHostClipboard` | 2 | core/src/lib/clipboard.ts:165 | opentui-core | todo |
| `createIcyStreamDemuxer` | 2 | core/src/audio-stream/icy/demuxer.ts:108 | opentui-core | todo |
| `createMarkdownCodeBlockRenderer` | **1** | core/src/renderables/Markdown.ts:171 | opentui-core::renderer | todo |
| `createRendererClipboardAdapter` | **1** | core/src/lib/clipboard.ts:308 | opentui-core::renderer | todo |
| `createSlotRegistry` | 2 | core/src/plugins/registry.ts:378 | opentui-core | todo |
| `createTerminalPalette` | 2 | core/src/lib/terminal-palette.ts:395 | opentui-core | todo |
| `createTextAttributes` | 2 | core/src/utils.ts:4 | opentui-core | todo |
| `createTimeline` | 2 | core/src/animation/Timeline.ts:589 | opentui-core | todo |
| `cyan` | 2 | core/src/lib/styled-text.ts:87 | opentui-core | todo |
| `decodePasteBytes` | **1** | core/src/lib/paste.ts:12 | opentui-core::renderer | todo |
| `defaultTextareaKeyBindings` | 2 | core/src/renderables/Textarea.ts:58 | opentui-core::renderer | todo |
| `delegate` | 2 | core/src/renderables/composition/vnode.ts:245 | opentui-core | todo |
| `destroyTreeSitterClient` | 2 | core/src/lib/tree-sitter/index.ts:62 | opentui-core | todo |
| `detectLinks` | 2 | core/src/lib/detect-links.ts:12 | opentui-core::ansi | todo |
| `dim` | 2 | core/src/lib/styled-text.ts:115 | opentui-core::ansi | todo |
| `engine` | 2 | core/src/animation/Timeline.ts:587 | opentui-core | todo |
| `env` | 2 | core/src/lib/env.ts:193 | opentui-core | todo |
| `envRegistry` | 2 | core/src/lib/env.ts:42 | opentui-core | todo |
| `extToFiletype` | 2 | core/src/lib/tree-sitter/resolve-ft.ts:146 | opentui-core | todo |
| `extensionToFiletype` | 2 | core/src/lib/tree-sitter/resolve-ft.ts:3 | opentui-core | todo |
| `fg` | **1** | core/src/lib/styled-text.ts:120 | opentui-core::color | todo |
| `fonts` | 2 | core/src/lib/ascii.font.ts:18 | opentui-core | todo |
| `generateEnvColored` | 2 | core/src/lib/env.ts:164 | opentui-core::color | todo |
| `generateEnvMarkdown` | 2 | core/src/lib/env.ts:134 | opentui-core | todo |
| `getBaseAttributes` | 2 | core/src/types.ts:28 | opentui-core | done |
| `getBorderFromSides` | 2 | core/src/lib/border.ts:125 | opentui-core | todo |
| `getBorderSides` | 2 | core/src/lib/border.ts:134 | opentui-core | todo |
| `getCharacterPositions` | 2 | core/src/lib/ascii.font.ts:155 | opentui-core | todo |
| `getDataPaths` | 2 | core/src/lib/data-paths.ts:107 | opentui-core | todo |
| `getLinkId` | 2 | core/src/utils.ts:50 | opentui-core::ansi | done |
| `getTreeSitterClient` | **1** | core/src/lib/tree-sitter/index.ts:22 | opentui-core | todo |
| `green` | 2 | core/src/lib/styled-text.ts:83 | opentui-core | todo |
| `h` | 2 | core/src/renderables/composition/vnode.ts:62 | opentui-core | todo |
| `hastToStyledText` | 2 | core/src/lib/hast-styled-text.ts:56 | opentui-core | todo |
| `hexToRgb` | 2 | core/src/lib/RGBA.ts:222 | opentui-core::color | done |
| `hsvToRgb` | 2 | core/src/lib/RGBA.ts:250 | opentui-core::color | done |
| `imageInfo` | 2 | core/src/image.ts:375 | opentui-core | todo |
| `infoStringToFiletype` | **1** | core/src/lib/tree-sitter/resolve-ft.ts:173 | opentui-core | todo |
| `instantiate` | 2 | core/src/renderables/composition/vnode.ts:187 | opentui-core | todo |
| `isEditBufferRenderable` | 2 | core/src/renderables/EditBufferRenderable.ts:51 | opentui-core::renderables | todo |
| `isRenderable` | 2 | core/src/Renderable.ts:132 | opentui-core::renderables | done |
| `isStyledText` | **1** | core/src/lib/styled-text.ts:22 | opentui-core | partial |
| `isTextNodeRenderable` | 2 | core/src/renderables/TextNode.ts:17 | opentui-core::renderables | todo |
| `isVNode` | 2 | core/src/renderables/composition/vnode.ts:132 | opentui-core | todo |
| `isValidBorderStyle` | 2 | core/src/lib/border.ts:22 | opentui-core | todo |
| `italic` | 2 | core/src/lib/styled-text.ts:112 | opentui-core::ansi | todo |
| `link` | 2 | core/src/lib/styled-text.ts:129 | opentui-core::ansi | todo |
| `magenta` | 2 | core/src/lib/styled-text.ts:86 | opentui-core | todo |
| `maybeMakeRenderable` | 2 | core/src/renderables/composition/vnode.ts:136 | opentui-core::renderables | todo |
| `measureText` | 2 | core/src/lib/ascii.font.ts:105 | opentui-core | todo |
| `nonAlphanumericKeys` | 2 | core/src/lib/parse.keypress.ts:125 | opentui-core::renderer | todo |
| `normalizeColorValue` | 2 | core/src/lib/RGBA.ts:217 | opentui-core::color | todo |
| `normalizeIndexedColorIndex` | 2 | core/src/lib/RGBA.ts:63 | opentui-core::color | done |
| `normalizeTerminalPalette` | 2 | core/src/lib/terminal-palette.ts:415 | opentui-core | todo |
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
| `parseKeypress` | 2 | core/src/lib/parse.keypress.ts:214 | opentui-core::renderer | todo |
| `parseLogLevel` | 2 | core/src/lib/yoga.options.ts:244 | opentui-core | todo |
| `parseMeasureMode` | 2 | core/src/lib/yoga.options.ts:266 | opentui-core::layout | todo |
| `parseOverflow` | 2 | core/src/lib/yoga.options.ts:282 | opentui-core::layout | todo |
| `parsePositionType` | 2 | core/src/lib/yoga.options.ts:298 | opentui-core::layout | todo |
| `parseUnit` | 2 | core/src/lib/yoga.options.ts:314 | opentui-core::layout | todo |
| `parseWrap` | 2 | core/src/lib/yoga.options.ts:332 | opentui-core::layout | todo |
| `pathToFiletype` | 2 | core/src/lib/tree-sitter/resolve-ft.ts:153 | opentui-core | todo |
| `red` | 2 | core/src/lib/styled-text.ts:82 | opentui-core | todo |
| `registerCorePlugin` | 2 | core/src/plugins/core-slot.ts:185 | opentui-core | todo |
| `registerEnvVar` | 2 | core/src/lib/env.ts:44 | opentui-core | todo |
| `renderFontToFrameBuffer` | 2 | core/src/lib/ascii.font.ts:220 | opentui-core::buffer | todo |
| `resolveBundledFilePath` | 2 | core/src/platform/runtime.ts:49 | opentui-core | todo |
| `resolveCoreSlot` | 2 | core/src/plugins/core-slot.ts:193 | opentui-core | todo |
| `resolveImageRenderProtocol` | 2 | core/src/renderables/Image.ts:20 | opentui-core | todo |
| `resolveRenderLib` | 2 | core/src/zig.ts:6938 | opentui-core | todo |
| `reverse` | 2 | core/src/lib/styled-text.ts:116 | opentui-core::ansi | todo |
| `rgbToHex` | 2 | core/src/lib/RGBA.ts:244 | opentui-core | todo |
| `setRenderLibPath` | 2 | core/src/zig.ts:6925 | opentui-core | todo |
| `setupAudio` | 2 | core/src/audio.ts:3685 | opentui-core | todo |
| `strikethrough` | 2 | core/src/lib/styled-text.ts:114 | opentui-core::ansi | todo |
| `stringToStyledText` | 2 | core/src/lib/styled-text.ts:36 | opentui-core | todo |
| `stripAnsiSequences` | 2 | core/src/lib/paste.ts:16 | opentui-core | todo |
| `t` | 2 | core/src/lib/styled-text.ts:150 | opentui-core | todo |
| `terminalNamedSingleStrokeKeys` | 2 | core/src/lib/parse.keypress.ts:127 | opentui-core::renderer | todo |
| `treeSitterToStyledText` | 2 | core/src/lib/tree-sitter-styled-text.ts:293 | opentui-core | todo |
| `treeSitterToTextChunks` | 2 | core/src/lib/tree-sitter-styled-text.ts:40 | opentui-core | todo |
| `underline` | 2 | core/src/lib/styled-text.ts:113 | opentui-core::ansi | todo |
| `visualizeRenderableTree` | 2 | core/src/utils.ts:55 | opentui-core::renderables | todo |
| `vstyles` | 2 | core/src/renderables/composition/constructs.ts:89 | opentui-core | todo |
| `white` | 2 | core/src/lib/styled-text.ts:88 | opentui-core | todo |
| `wrapWithDelegates` | 2 | core/src/renderables/composition/vnode.ts:148 | opentui-core | todo |
| `yellow` | 2 | core/src/lib/styled-text.ts:84 | opentui-core | todo |

### solid (48 runtime exports)

| symbol | tier | upstream file:line | Rust target | status |
|---|---|---|---|---|
| `BoldSpanRenderable` | 2 | solid/src/elements/catalogue.ts:49 | opentui-solid | todo |
| `Dynamic` | 2 | solid/src/elements/extras.ts:105 | opentui-solid | todo |
| `ItalicSpanRenderable` | 2 | solid/src/elements/catalogue.ts:55 | opentui-solid | todo |
| `LayoutSlotRenderable` | 2 | solid/src/elements/slot.ts:91 | opentui-solid | todo |
| `LineBreakRenderable` | 2 | solid/src/elements/catalogue.ts:67 | opentui-solid | todo |
| `LinkRenderable` | 2 | solid/src/elements/catalogue.ts:82 | opentui-solid | todo |
| `Portal` | **1** | solid/src/elements/extras.ts:14 | opentui-solid | partial |
| `RendererContext` | 2 | solid/src/elements/hooks.ts:12 | opentui-solid | todo |
| `Slot` | 2 | solid/src/plugins/slot.tsx:81 | opentui-solid | todo |
| `SlotRenderable` | 2 | solid/src/elements/slot.ts:162 | opentui-solid | todo |
| `TextSlotRenderable` | 2 | solid/src/elements/slot.ts:54 | opentui-solid | todo |
| `TimeToFirstDraw` | **1** | solid/src/time-to-first-draw.tsx:15 | opentui-solid | todo |
| `UnderlineSpanRenderable` | 2 | solid/src/elements/catalogue.ts:61 | opentui-solid | todo |
| `_render` | 2 | solid-js re-export | opentui-solid | todo |
| `baseComponents` | 2 | solid/src/elements/catalogue.ts:92 | opentui-solid | todo |
| `componentCatalogue` | 2 | solid/src/elements/catalogue.ts:119 | opentui-solid | todo |
| `createComponent` | 2 | solid-js re-export | opentui-solid | todo |
| `createDynamic` | 2 | solid/src/elements/extras.ts:75 | opentui-solid | todo |
| `createElement` | 2 | solid-js re-export | opentui-solid | todo |
| `createScrollbackWriter` | **1** | solid/src/scrollback.ts:145 | opentui-solid | todo |
| `createSlot` | 2 | solid/src/plugins/slot.tsx:66 | opentui-solid | todo |
| `createSlotNode` | 2 | solid/src/reconciler.ts:155 | opentui-solid | todo |
| `createSolidSlotRegistry` | 2 | solid/src/plugins/slot.tsx:56 | opentui-solid | todo |
| `createTextNode` | 2 | solid-js re-export | opentui-solid | todo |
| `effect` | 2 | solid-js re-export | opentui-solid | todo |
| `extend` | **1** | solid/src/elements/catalogue.ts:133 | opentui-solid | partial |
| `getComponentCatalogue` | 2 | solid/src/elements/catalogue.ts:137 | opentui-solid | todo |
| `insert` | 2 | solid-js re-export | opentui-solid | todo |
| `insertNode` | 2 | solid-js re-export | opentui-solid | todo |
| `memo` | 2 | solid-js re-export | opentui-solid | todo |
| `mergeProps` | 2 | solid-js re-export | opentui-solid | todo |
| `onBlur` | 2 | solid/src/elements/hooks.ts:120 | opentui-solid | todo |
| `onFocus` | 2 | solid/src/elements/hooks.ts:108 | opentui-solid | todo |
| `onResize` | 2 | solid/src/elements/hooks.ts:24 | opentui-solid | todo |
| `render` | **1** | — | opentui-solid | done |
| `setProp` | 2 | solid-js re-export | opentui-solid | todo |
| `spread` | 2 | solid-js re-export | opentui-solid | todo |
| `testRender` | 2 | — | opentui-solid | todo |
| `textNodeKeys` | 2 | solid/src/elements/catalogue.ts:31 | opentui-solid | todo |
| `use` | 2 | solid-js re-export | opentui-solid | todo |
| `useKeyHandler` | 2 | solid/src/elements/hooks.ts:106 | opentui-solid | todo |
| `useKeyboard` | **1** | solid/src/elements/hooks.ts:73 | opentui-solid | partial |
| `usePaste` | **1** | solid/src/elements/hooks.ts:91 | opentui-solid | partial |
| `useRenderer` | **1** | solid/src/elements/hooks.ts:14 | opentui-solid | done |
| `useSelectionHandler` | 2 | solid/src/elements/hooks.ts:132 | opentui-solid | todo |
| `useTerminalDimensions` | **1** | solid/src/elements/hooks.ts:36 | opentui-solid | done |
| `useTimeline` | 2 | solid/src/elements/hooks.ts:144 | opentui-solid | todo |
| `writeSolidToScrollback` | 2 | solid/src/scrollback.ts:221 | opentui-solid | todo |

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
