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
  TextRenderable,
  BoxRenderable,
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

const scenarioPath = process.argv[2]
const outPath = process.argv[3]
if (!scenarioPath || !outPath) {
  console.error("usage: bun js/driver.ts <scenario.json> <out.json>")
  process.exit(2)
}
const scenario = await Bun.file(scenarioPath).json()

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

// scenario steps → real renderables
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
  const parent = node(step.parent ?? "root")
  let r
  if (step.op === "text") {
    const o = step.options ?? {}
    r = new TextRenderable(renderer, {
      content: o.content ?? "",
      ...(o.fg !== undefined ? { fg: o.fg } : {}),
      ...(o.bg !== undefined ? { bg: o.bg } : {}),
      ...(o.width !== undefined ? { width: o.width } : {}),
      ...(o.height !== undefined ? { height: o.height } : {}),
      ...(o.wrapMode !== undefined ? { wrapMode: o.wrapMode } : {}),
    })
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
  } else if (step.op === "flush") {
    await setup.flush()
    continue
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

const out = {
  oracle: "@opentui/core@0.5.14",
  renderer: { width: buf.width, height: buf.height },
  lines: spans,
  charFrame: setup.captureCharFrame(),
  sgrBytes: Array.from(stdout.bytes()),
}
await Bun.write(outPath, JSON.stringify(out))
console.log(`oracle: ${out.renderer.width}x${out.renderer.height}, ${out.sgrBytes.length} SGR bytes -> ${outPath}`)

renderer.destroy()
process.exit(0)
