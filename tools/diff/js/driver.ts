// opentui-diff JS driver — runs the REAL @opentui 0.5.14 (the pinned spec)
// against a scenario and dumps the oracle capture:
//   * cell-grid spans (fg/bg packed u16x4 + attributes, exact words)
//   * char frame
//   * raw SGR byte stream (stdout writes, recorded)
//
// Usage: bun js/driver.ts <scenario.json> <out.json>
import { Writable } from "node:stream"
import {
  createCliRenderer,
  InputRenderable,
  TextRenderable,
  BoxRenderable,
  TextareaRenderable,
  TextNodeRenderable,
  StyledText,
  parseColor,
  ScrollBoxRenderable,
  type Renderable,
} from "@opentui/core"
import { createTestRenderer } from "@opentui/core/testing"

class RecordingStream extends Writable {
  isTTY = true
  columns
  rows
  chunks = []
  constructor(columns, rows) {
    super()
    this.columns = columns
    this.rows = rows
  }
  _write(chunk, _encoding, callback) {
    this.chunks.push(Buffer.from(chunk))
    callback()
  }
  getColorDepth() {
    return 24
  }
  bytes() {
    return Buffer.concat(this.chunks)
  }
}


// textarea text-op vocabulary — mirrors the Rust scenario runner 1:1
// (the EditBufferRenderable/Textarea public surface)
function textareaOp(ta, op) {
  switch (op.op) {
    case "setText":
      ta.setText(op.text)
      break
    case "replaceText":
      ta.replaceText(op.text)
      break
    case "insertText":
      ta.insertText(op.text)
      break
    case "insertChar":
      ta.insertChar(op.text)
      break
    case "setCursor":
      ta.setCursor(op.row, op.col)
      break
    case "setCursorByOffset":
      ta.cursorOffset = op.offset
      break
    case "moveCursorLeft":
      ta.moveCursorLeft({ select: !!op.select })
      break
    case "moveCursorRight":
      ta.moveCursorRight({ select: !!op.select })
      break
    case "moveCursorUp":
      ta.moveCursorUp({ select: !!op.select })
      break
    case "moveCursorDown":
      ta.moveCursorDown({ select: !!op.select })
      break
    case "moveWordForward":
      ta.moveWordForward({ select: !!op.select })
      break
    case "moveWordBackward":
      ta.moveWordBackward({ select: !!op.select })
      break
    case "gotoLine":
      ta.gotoLine(op.line)
      break
    case "gotoLineHome":
      ta.gotoLineHome({ select: !!op.select })
      break
    case "gotoLineEnd":
      ta.gotoLineEnd({ select: !!op.select })
      break
    case "gotoVisualLineHome":
      ta.gotoVisualLineHome({ select: !!op.select })
      break
    case "gotoVisualLineEnd":
      ta.gotoVisualLineEnd({ select: !!op.select })
      break
    case "gotoBufferHome":
      ta.gotoBufferHome({ select: !!op.select })
      break
    case "gotoBufferEnd":
      ta.gotoBufferEnd({ select: !!op.select })
      break
    case "selectAll":
      ta.selectAll()
      break
    case "deleteChar":
      ta.deleteChar()
      break
    case "deleteCharBackward":
      ta.deleteCharBackward()
      break
    case "deleteLine":
      ta.deleteLine()
      break
    case "deleteToLineEnd":
      ta.deleteToLineEnd()
      break
    case "deleteToLineStart":
      ta.deleteToLineStart()
      break
    case "deleteWordForward":
      ta.deleteWordForward()
      break
    case "deleteWordBackward":
      ta.deleteWordBackward()
      break
    case "newLine":
      ta.newLine()
      break
    case "deleteRange":
      ta.deleteRange(op.startRow, op.startCol, op.endRow, op.endCol)
      break
    case "undo":
      ta.undo()
      break
    case "redo":
      ta.redo()
      break
    case "clear":
      ta.clear()
      break
    case "setSelection":
      ta.setSelection(op.start, op.end)
      break
    case "setSelectionInclusive":
      ta.setSelectionInclusive(op.start, op.end)
      break
    case "updateSelection":
      ta.editorView.updateSelection(op.end, undefined, undefined)
      break
    case "clearSelection":
      ta.clearSelection()
      break
    case "deleteSelection":
      ta.deleteSelection()
      break
    case "setLocalSelection":
      ta.editorView.setLocalSelection(
        op.anchorX, op.anchorY, op.focusX, op.focusY,
        undefined, undefined, !!op.updateCursor, !!op.followCursor, op.behavior ?? "cell",
      )
      break
    case "updateLocalSelection":
      ta.editorView.updateLocalSelection(
        op.anchorX, op.anchorY, op.focusX, op.focusY,
        undefined, undefined, !!op.updateCursor, !!op.followCursor, op.behavior ?? "cell",
      )
      break
    case "setViewport": {
      const vp = ta.editorView.getViewport()
      ta.editorView.setViewport(op.x ?? 0, op.y ?? 0, op.width ?? vp.width, op.height ?? vp.height, op.moveCursor !== false)
      break
    }
    case "scrollBy":
      {
        // EditBufferRenderable.handleScroll semantics (450-479)
        const vp = ta.editorView.getViewport()
        if (op.direction === "up") {
          const newOffsetY = Math.max(0, vp.offsetY - op.delta)
          ta.editorView.setViewport(vp.offsetX, newOffsetY, vp.width, vp.height, true)
        } else if (op.direction === "down") {
          const total = ta.editorView.getTotalVirtualLineCount()
          const maxOffsetY = Math.max(0, total - vp.height)
          const newOffsetY = Math.min(vp.offsetY + op.delta, maxOffsetY)
          ta.editorView.setViewport(vp.offsetX, newOffsetY, vp.width, vp.height, true)
        }
      }
      break
    case "focus":
      ta.focus()
      break
    case "blur":
      ta.blur()
      break
    case "placeholder":
      ta.placeholder = op.value ?? null
      break
    case "key":
      ta.handleKeyPress({
        name: op.name,
        ctrl: !!op.ctrl,
        shift: !!op.shift,
        meta: !!op.meta,
        super: !!op.super,
        hyper: false,
        sequence: op.sequence,
      })
      break
    case "setWrapMode":
      ta.wrapMode = op.mode
      break
    default:
      throw new Error(`unknown textarea op: ${op.op}`)
  }
}

const scenarioPath = process.argv[2]
const outPath = process.argv[3]
if (!scenarioPath || !outPath) {
  console.error("usage: bun js/driver.ts <scenario.json> <out.json>")
  process.exit(2)
}
const scenario = await Bun.file(scenarioPath).json()

// Attribute names → bits — createTextAttributes vocabulary (utils.ts:4).
const ATTR_BITS = {
  bold: 1,
  dim: 1 << 1,
  italic: 1 << 2,
  underline: 1 << 3,
  blink: 1 << 4,
  inverse: 1 << 5,
  reverse: 1 << 5,
  hidden: 1 << 6,
  strikethrough: 1 << 7,
}
function parseAttributes(v) {
  if (v === undefined || v === null) return 0
  if (typeof v === "number") return v
  if (Array.isArray(v)) return v.reduce((bits, name) => bits | (ATTR_BITS[name] ?? 0), 0)
  throw new Error(`attributes must be number or name array: ${JSON.stringify(v)}`)
}

// chunk JSON → TextChunk (fg/bg parsed like the port's parse_color)
function parseChunk(v) {
  if (typeof v !== "object" || v === null || typeof v.text !== "string") {
    throw new Error(`chunk needs text: ${JSON.stringify(v)}`)
  }
  const chunk = { __isChunk: true, text: v.text, attributes: parseAttributes(v.attributes) }
  if (v.fg !== undefined) chunk.fg = parseColor(v.fg)
  if (v.bg !== undefined) chunk.bg = parseColor(v.bg)
  return chunk
}

function parseStyledContent(v) {
  if (v === undefined || v === null) return undefined
  // strings pass through: TextRenderable's `_hasManualStyledText`
  // (Text.ts:32) treats an empty string as NOT manual (tree wins)
  if (typeof v === "string") return v
  if (Array.isArray(v)) return new StyledText(v.map(parseChunk))
  throw new Error(`content must be string or chunk array`)
}

function parseNodeTree(v, id) {
  const options = {}
  if (v.fg !== undefined) options.fg = v.fg
  if (v.bg !== undefined) options.bg = v.bg
  if (v.attributes !== undefined) options.attributes = parseAttributes(v.attributes)
  const node = new TextNodeRenderable(options)
  for (const child of v.children ?? []) {
    if (typeof child === "string") node.add(child)
    else node.add(parseNodeTree(child, `${id}-c`))
  }
  return node
}

// A recording stdout so the SGR byte stream is captured. We boot through
// upstream's own createTestRenderer (deterministic headless profile) but
// hand it our stream via config.stdout passthrough.
const stdout = new RecordingStream(scenario.width, scenario.height)
const setup = await createTestRenderer({
  width: scenario.width,
  height: scenario.height,
  stdout,
  // route native frames through the span feed → stdout (our recorder),
  // NOT the in-memory destination setupTestRenderer defaults to
  bufferedOutput: "buffer",
  // deterministic profile (createTestRenderer defaults, restated):
  screenMode: "main-screen",
  consoleMode: "disabled",
  externalOutputMode: "passthrough",
  exitOnCtrlC: false,
})
const renderer = setup.renderer

// scenario steps → real renderables, IN ORDER (mount, focus, key feed)
const nodes = new Map()
function node(id) {
  if (id === "root") return renderer.root
  return nodes.get(id)
}
function applyProps(target, o) {
  // The generic property vocabulary both sides understand: layout-ish box
  // options in option bags; see each op for what it forwards.
  const out = {}
  if (o.flexDirection !== undefined) out.flexDirection = o.flexDirection
  if (o.flexGrow !== undefined) out.flexGrow = o.flexGrow
  if (o.padding !== undefined) out.padding = o.padding
  if (o.paddingTop !== undefined) out.paddingTop = o.paddingTop
  if (o.paddingBottom !== undefined) out.paddingBottom = o.paddingBottom
  if (o.paddingLeft !== undefined) out.paddingLeft = o.paddingLeft
  if (o.paddingRight !== undefined) out.paddingRight = o.paddingRight
  return out
}
for (const step of scenario.steps) {
  if (step.op === "set") {
    // Generic scripted mutation: node[step.prop] = step.value
    const target = node(step.id)
    if (!target) throw new Error(`set: unknown node ${step.id}`)
    target[step.prop] = step.value
    continue
  }
  if (step.op === "call") {
    // Generic scripted invocation: node[step.method](...step.args)
    const target = node(step.id)
    if (!target) throw new Error(`call: unknown node ${step.id}`)
    target[step.method](...(step.args ?? []))
    continue
  }
  if (step.op === "resize") {
    await renderer.resize(step.width, step.height)
    stdout.columns = step.width
    stdout.rows = step.height
    continue
  }
  if (step.op === "flush") {
    await setup.flush()
    continue
  }
  if (step.op === "frame") {
    // one deterministic render tick (multi-frame textarea scenarios)
    await setup.renderOnce()
    continue
  }
  if (step.op === "textarea") {
    const o = step.options ?? {}
    const options = {}
    if (o.width !== undefined) options.width = o.width
    if (o.height !== undefined) options.height = o.height
    if (o.initialValue !== undefined) options.initialValue = o.initialValue
    if (o.textColor !== undefined) options.textColor = o.textColor
    if (o.backgroundColor !== undefined) options.backgroundColor = o.backgroundColor
    if (o.focusedTextColor !== undefined) options.focusedTextColor = o.focusedTextColor
    if (o.focusedBackgroundColor !== undefined) options.focusedBackgroundColor = o.focusedBackgroundColor
    if (o.placeholder !== undefined) options.placeholder = o.placeholder
    if (o.placeholderColor !== undefined) options.placeholderColor = o.placeholderColor
    if (o.selectionBg !== undefined) options.selectionBg = o.selectionBg
    if (o.selectionFg !== undefined) options.selectionFg = o.selectionFg
    if (o.wrapMode !== undefined) options.wrapMode = o.wrapMode
    if (o.showCursor !== undefined) options.showCursor = o.showCursor
    if (o.cursorColor !== undefined) options.cursorColor = o.cursorColor
    if (o.scrollMargin !== undefined) options.scrollMargin = o.scrollMargin
    const parent = node(step.parent ?? "root")
    const r = new TextareaRenderable(renderer, options)
    parent.add(r)
    nodes.set(step.id, r)
    for (const op of step.textOps ?? []) textareaOp(r, op)
    continue
  }
  if (step.op === "ta-op") {
    const target = nodes.get(step.target ?? step.id)
    if (!target) throw new Error(`ta-op target ${step.target ?? step.id} not found`)
    for (const op of step.textOps ?? []) textareaOp(target, op)
    continue
  }
  if (step.op === "focus") {
    const target = node(step.id)
    if (!target) throw new Error(`focus: unknown id ${step.id}`)
    target.focus()
    continue
  }
  if (step.op === "key") {
    // raw bytes through the REAL stdin parser -> key handler pipeline
    renderer.stdin.emit("data", Buffer.from(Uint8Array.from(step.sequence ?? [])))
    continue
  }
  if (step.op === "text-node") {
    // attach a TextNodeRenderable to an existing text renderable
    const target = nodes.get(step.parent)
    if (!(target instanceof TextRenderable)) {
      throw new Error(`text-node parent ${step.parent} is not a TextRenderable`)
    }
    const o = step.options ?? {}
    const options = {}
    if (o.fg !== undefined) options.fg = o.fg
    if (o.bg !== undefined) options.bg = o.bg
    if (o.attributes !== undefined) options.attributes = parseAttributes(o.attributes)
    const n = new TextNodeRenderable(options)
    if (step.text !== undefined) n.add(step.text)
    target.add(n)
    continue
  }
  if (step.op === "text-add") {
    const target = nodes.get(step.target)
    if (!(target instanceof TextRenderable)) {
      throw new Error(`text-add target ${step.target} is not a TextRenderable`)
    }
    if (step.text === undefined) throw new Error("text-add needs text")
    target.add(step.text)
    continue
  }

  const parent = node(step.parent ?? "root")
  if (!parent) throw new Error(`parent ${step.parent} not found for ${step.id}`)
  let r
  if (step.op === "text") {
    const o = step.options ?? {}
    const options = {
      ...(o.content !== undefined ? { content: parseStyledContent(o.content) } : {}),
      ...(o.fg !== undefined ? { fg: o.fg } : {}),
      ...(o.bg !== undefined ? { bg: o.bg } : {}),
      ...(o.attributes !== undefined ? { attributes: parseAttributes(o.attributes) } : {}),
      ...(o.width !== undefined ? { width: o.width } : {}),
      ...(o.height !== undefined ? { height: o.height } : {}),
      ...(o.wrapMode !== undefined ? { wrapMode: o.wrapMode } : {}),
      ...(o.textAlign !== undefined ? { textAlign: o.textAlign } : {}),
      ...(o.tabIndicator !== undefined ? { tabIndicator: o.tabIndicator } : {}),
      ...(o.tabIndicatorColor !== undefined ? { tabIndicatorColor: o.tabIndicatorColor } : {}),
      ...(o.truncate !== undefined ? { truncate: o.truncate } : {}),
    }
    r = new TextRenderable(renderer, options)
    if (o.nodeContent !== undefined) {
      r.add(parseNodeTree(o.nodeContent, `${step.id}-node`))
    }
  } else if (step.op === "box") {
    const o = step.options ?? {}
    r = new BoxRenderable(renderer, {
      ...(o.backgroundColor !== undefined ? { backgroundColor: o.backgroundColor } : {}),
      ...(o.border !== undefined ? { border: o.border } : {}),
      ...(o.borderStyle !== undefined ? { borderStyle: o.borderStyle } : {}),
      ...(o.borderColor !== undefined ? { borderColor: o.borderColor } : {}),
      ...(o.shouldFill !== undefined ? { shouldFill: o.shouldFill } : {}),
      ...(o.title !== undefined ? { title: o.title } : {}),
      ...(o.titleAlignment !== undefined ? { titleAlignment: o.titleAlignment } : {}),
      ...(o.width !== undefined ? { width: o.width } : {}),
      ...(o.height !== undefined ? { height: o.height } : {}),
      ...(o.flexDirection !== undefined ? { flexDirection: o.flexDirection } : {}),
      ...(o.padding !== undefined ? { padding: o.padding } : {}),
    })
  } else if (step.op === "scrollbox") {
    const o = step.options ?? {}
    const boxish = {
      ...(o.backgroundColor !== undefined ? { backgroundColor: o.backgroundColor } : {}),
      ...(o.border !== undefined ? { border: o.border } : {}),
      ...(o.borderColor !== undefined ? { borderColor: o.borderColor } : {}),
      ...(o.borderStyle !== undefined ? { borderStyle: o.borderStyle } : {}),
      ...(o.shouldFill !== undefined ? { shouldFill: o.shouldFill } : {}),
      ...(o.width !== undefined ? { width: o.width } : {}),
      ...(o.height !== undefined ? { height: o.height } : {}),
      ...(o.scrollX !== undefined ? { scrollX: o.scrollX } : {}),
      ...(o.scrollY !== undefined ? { scrollY: o.scrollY } : {}),
      ...(o.stickyScroll !== undefined ? { stickyScroll: o.stickyScroll } : {}),
      ...(o.stickyStart !== undefined ? { stickyStart: o.stickyStart } : {}),
      ...applyProps(null, o),
    }
    const sub = (name) => (o[name] === undefined ? {} : applyProps(null, o[name]))
    const track = o.scrollbarOptions ?? {}
    // slider: foregroundColor = thumb color, backgroundColor = track color
    const trackOpts = {
      ...(track.thumbColor !== undefined ? { foregroundColor: track.thumbColor } : {}),
      ...(track.trackColor !== undefined ? { backgroundColor: track.trackColor } : {}),
    }
    r = new ScrollBoxRenderable(renderer, {
      ...boxish,
      ...(Object.keys(trackOpts).length ? { scrollbarOptions: { trackOptions: trackOpts } } : {}),
      ...(o.viewportOptions !== undefined ? { viewportOptions: sub("viewportOptions") } : {}),
      ...(o.contentOptions !== undefined ? { contentOptions: sub("contentOptions") } : {}),
      ...(o.wrapperOptions !== undefined ? { wrapperOptions: sub("wrapperOptions") } : {}),
    })
  } else if (step.op === "input") {
    const o = step.options ?? {}
    r = new InputRenderable(renderer, {
      id: step.id,
      ...(o.value !== undefined ? { value: o.value } : {}),
      ...(o.placeholder !== undefined ? { placeholder: o.placeholder } : {}),
      ...(o.width !== undefined ? { width: o.width } : {}),
      ...(o.maxLength !== undefined ? { maxLength: o.maxLength } : {}),
      ...(o.textColor !== undefined ? { textColor: o.textColor } : {}),
      ...(o.focusedTextColor !== undefined ? { focusedTextColor: o.focusedTextColor } : {}),
      ...(o.backgroundColor !== undefined ? { backgroundColor: o.backgroundColor } : {}),
      ...(o.placeholderColor !== undefined ? { placeholderColor: o.placeholderColor } : {}),
    })
  } else {
    throw new Error(`unknown op: ${step.op}`)
  }
  // ScrollBox routes child adds into its content viewport (ScrollBox.add).
  parent.add(r)
  nodes.set(step.id, r)
}

// deterministic settle: run the loop until visually idle (upstream flush)
await setup.flush()
await setup.renderOnce()

const buf = renderer.currentRenderBuffer
const spans = buf.getSpanLines().map((line) => ({
  spans: line.spans.map((s) => ({
    text: s.text,
    fg: Array.from(s.fg.buffer),
    bg: Array.from(s.bg.buffer),
    attributes: s.attributes,
    width: s.width,
  })),
}))

// terminal cursor: the frame's cursor CUP + show sequence
// (setCursorPosition writes `ESC [<row>;<col> H` before `ESC[?25h`)
const cursorMatch = [...stdout.bytes().toString("latin1").matchAll(/\x1b\[(\d+);(\d+)H\x1b\[\?25h/g)]

const out = {
  oracle: "@opentui/core@0.5.14",
  renderer: { width: buf.width, height: buf.height },
  lines: spans,
  charFrame: setup.captureCharFrame(),
  sgrBytes: Array.from(stdout.bytes()),
  cursor: cursorMatch.length
    ? {
        row: Number(cursorMatch[cursorMatch.length - 1][1]),
        col: Number(cursorMatch[cursorMatch.length - 1][2]),
        visible: true,
      }
    : { row: 0, col: 0, visible: false },
}
await Bun.write(outPath, JSON.stringify(out))
console.log(`oracle: ${out.renderer.width}x${out.renderer.height}, ${out.sgrBytes.length} SGR bytes -> ${outPath}`)

renderer.destroy()
process.exit(0)
