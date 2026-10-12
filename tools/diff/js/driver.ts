// opentui-diff JS driver — runs the REAL @opentui 0.5.14 (the pinned spec)
// against a scenario and dumps the oracle capture:
//   * cell-grid spans (fg/bg packed u16x4 + attributes, exact words)
//   * char frame
//   * raw SGR byte stream (stdout writes, recorded)
//
// Usage: bun js/driver.ts <scenario.json> <out.json>
import { Writable } from "node:stream"
import { createCliRenderer, TextRenderable, BoxRenderable, type Renderable } from "@opentui/core"
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
for (const step of scenario.steps) {
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
  } else {
    throw new Error(`unknown op: ${step.op}`)
  }
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
