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
  TextNodeRenderable,
  StyledText,
  parseColor,
  ScrollBoxRenderable,
  type Renderable,
} from "@opentui/core"
import { createTestRenderer } from "@opentui/core/testing"

// solid-js must resolve its CLIENT build (the "node" condition serves
// the SSR stubs) — tools/diff/js/ensure-solid-client-build.sh patches
// the installed tree before every oracle capture.
const {
  testRender,
  createElement,
  spread,
  insertNode,
  setProp,
  insert,
  Portal,
  createSlotNode,
  extend,
} = await import("@opentui/solid")
const { createSignal, createComponent, createRoot, Show, For, runWithOwner, getOwner } = await import(
  "solid-js"
)

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

// ---------------------------------------------------------------------------
// solid mode: drive the REAL @opentui/solid reconciler (solid-js
// Show/For + createElement/spread/insertNode/setProp) — the oracle for
// the opentui-solid port. The initial testRender component captures
// the solid owner (the Portal pattern, extras.ts:19,25) so later steps
// can create elements/effects under the RendererContext.
// ---------------------------------------------------------------------------
if (scenario.mode === "solid") {
  const stdout = new RecordingStream(scenario.width, scenario.height)
  let owner
  const setup = await testRender(() => {
    owner = getOwner()
    return null
  }, {
    width: scenario.width,
    height: scenario.height,
    stdout,
    bufferedOutput: "buffer",
    screenMode: "main-screen",
    consoleMode: "disabled",
    externalOutputMode: "passthrough",
    exitOnCtrlC: false,
  })
  const renderer = setup.renderer

  // the differential's custom-element fixture: a plain BoxRenderable
  // subclass registered through extend() (catalogue.ts:133-135) — the
  // Rust driver registers the box-backed factory equivalent
  class BadgeBox extends BoxRenderable {}
  extend({ badge_box: BadgeBox })

  const nodes = new Map() // id -> { node, parentId, dispose? }
  const signals = new Map() // id -> [get, set]
  const lists = new Map() // id -> [get, set] (arrays of pooled items)
  const pools = new Map() // listId -> Map(key -> pooled {key, value} object)

  const nodeOf = (id) => {
    // mounted elements shadow the reserved "root" name
    if (nodes.has(id)) return nodes.get(id).node
    if (id === "root") return renderer.root
    throw new Error(`unknown node ${id}`)
  }

  // JSON value → prop value; {"$signal"} binds reactively via a getter
  // (the compiled-JSX getter-prop form spread's render effect reads)
  const propValue = (v, itemValue) => {
    if (v && typeof v === "object" && !Array.isArray(v) && "$signal" in v) return undefined
    if (v === "$item") return itemValue
    return v
  }

  const buildProps = (options, itemValue) => {
    const out = {}
    for (const [k, v] of Object.entries(options ?? {})) {
      if (v && typeof v === "object" && !Array.isArray(v) && "$signal" in v) {
        const [get] = signals.get(v.$signal)
        Object.defineProperty(out, k, { get: () => get(), enumerable: true })
      } else {
        out[k] = propValue(v, itemValue)
      }
    }
    return out
  }

  // one el-spec → a live node (createElement + spread + no insert)
  const buildElement = (spec, itemValue) => {
    const el = runWithOwner(owner, () => createElement(spec.tag))
    const props = buildProps(spec.options, itemValue)
    runWithOwner(owner, () => spread(el, props))
    return el
  }

  // one el-spec → a live SUBTREE (children mounted into the node)
  const buildSubtree = (spec, itemValue) => {
    const el = buildElement(spec, itemValue)
    if (spec.id) nodes.set(spec.id, { node: el, parentId: spec.__parentId ?? "root" })
    for (const kid of spec.children ?? []) {
      const child = buildSubtree(kid, itemValue)
      insertNode(el, child)
    }
    return el
  }

  const mountSpec = (spec, parentId, anchorId) => {
    const el = buildElement(spec, undefined)
    const parent = nodeOf(parentId ?? "root")
    const anchor = anchorId ? nodeOf(anchorId) : undefined
    insertNode(parent, el, anchor)
    if (spec.id) nodes.set(spec.id, { node: el, parentId: parentId ?? "root" })
    for (const kid of spec.children ?? []) {
      mountSpec(kid, spec.id, undefined)
    }
    return el
  }

  // SOLID_DBG=1 dumps the frame at each flush (scenario debugging)
  const dumpStep = (label) => {
    if (!process.env.SOLID_DBG) return
    console.error(`[${label}] frame:`, JSON.stringify(setup.captureCharFrame()))
  }

  for (const step of scenario.steps) {
    if (step.op === "signal") {
      const value = step.value ?? step.options?.value
      if (Array.isArray(value) && value.every((e) => e && typeof e === "object" && "key" in e)) {
        if (!lists.has(step.id)) lists.set(step.id, createSignal([]))
        if (!pools.has(step.id)) pools.set(step.id, new Map())
        const pool = pools.get(step.id)
        const [, set] = lists.get(step.id)
        // keyed identity: reuse the pooled object when the payload is
        // unchanged (For keeps its node); a changed payload is a NEW
        // identity (For remounts the item)
        set(
          value.map(({ key, value: v }) => {
            const prev = pool.get(key)
            if (prev && prev.value === v) return prev
            const obj = { key, value: v }
            pool.set(key, obj)
            return obj
          }),
        )
      } else {
        if (!signals.has(step.id)) signals.set(step.id, createSignal(value))
        else signals.get(step.id)[1](value)
      }
      continue
    }
    if (step.op === "el") {
      if (step.tag === "portal") {
        // upstream Portal (extras.ts:14-55): children render into
        // `mount` (a live node), a marker occupies the JSX position.
        // A disposable root scopes the portal — disposing it runs
        // Portal's onCleanup (container removal, extras.ts:39-48),
        // the owner-disposal path a hosting Show/For provides in an
        // app.
        const parent = nodeOf(step.parent ?? "root")
        const mountId = step.options?.mount ?? "root"
        const specs = step.children ?? []
        let dispose
        const marker = runWithOwner(owner, () =>
          createRoot((d) => {
            dispose = d
            return createComponent(Portal, {
              mount: nodeOf(mountId),
              children: () => specs.map((spec) => buildSubtree(spec, undefined)),
            })
          }),
        )
        insertNode(parent, marker)
        nodes.set(step.id, { node: marker, parentId: step.parent ?? "root", dispose })
        continue
      }
      const spec = { tag: step.tag, id: step.id, options: step.options, children: step.children }
      mountSpec(spec, step.parent ?? "root", step.anchor ?? undefined)
      continue
    }
    if (step.op === "prop") {
      const target = nodeOf(step.id)
      if (!target) throw new Error(`prop: unknown node ${step.id}`)
      const value = step.value ?? step.options?.value
      setProp(target, step.prop, propValue(value, undefined))
      continue
    }
    if (step.op === "show") {
      const [get] = signals.get(step.signal)
      const parent = nodeOf(step.parent ?? "root")
      const specs = step.children ?? []
      // the compiled-JSX control-flow shape: a slot marker anchors the
      // insertion (insert(parent, accessor, marker)) so the reconciler
      // reconciles against the marker instead of replacing the first
      // child (universal.js:75-84's single-value path)
      const marker = runWithOwner(owner, () => createSlotNode())
      insertNode(parent, marker)
      runWithOwner(owner, () => {
        insert(
          parent,
          createComponent(Show, {
            get when() {
              return get() === true
            },
            children: () => specs.map((spec) => buildSubtree(spec, undefined)),
          }),
          marker,
        )
      })
      continue
    }
    if (step.op === "for") {
      const [get] = lists.get(step.signal)
      const parent = nodeOf(step.parent ?? "root")
      const template = step.item
      runWithOwner(owner, () => {
        insert(
          parent,
          createComponent(For, {
            get each() {
              return get()
            },
            children: (item) => buildSubtree(template, item.value),
          }),
        )
      })
      continue
    }
    if (step.op === "el-remove") {
      // upstream `_removeNode` (reconciler.ts:113-141 — not exported
      // by the package): parent.remove + deferred destroyRecursively;
      // a disposable scope (portals) tears down first — owner
      // disposal runs onCleanup (the container removal)
      const entry = nodes.get(step.id)
      if (!entry) throw new Error(`el-remove: unknown node ${step.id}`)
      entry.dispose?.()
      const parent = nodeOf(entry.parentId)
      parent.remove(entry.node)
      setTimeout(() => {
        if (!entry.node.parent) entry.node.destroyRecursively()
      }, 0)
      nodes.delete(step.id)
      continue
    }
    if (step.op === "flush") {
      await setup.flush()
      dumpStep("flush")
      continue
    }
    if (step.op === "resize") {
      await renderer.resize(step.width, step.height)
      stdout.columns = step.width
      stdout.rows = step.height
      continue
    }
    throw new Error(`unknown solid op: ${step.op}`)
  }

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
    oracle: "@opentui/solid@0.5.14",
    renderer: { width: buf.width, height: buf.height },
    lines: spans,
    charFrame: setup.captureCharFrame(),
    sgrBytes: Array.from(stdout.bytes()),
  }
  await Bun.write(outPath, JSON.stringify(out))
  console.log(`oracle: ${out.renderer.width}x${out.renderer.height}, ${out.sgrBytes.length} SGR bytes -> ${outPath}`)
  renderer.destroy()
  process.exit(0)
}


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
    const r = new TextRenderable(renderer, options)
    if (o.nodeContent !== undefined) {
      r.add(parseNodeTree(o.nodeContent, `${step.id}-node`))
    }
    parent.add(r)
    nodes.set(step.id, r)
  } else if (step.op === "box") {
    const o = step.options ?? {}
    const r = new BoxRenderable(renderer, {
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
    parent.add(r)
    nodes.set(step.id, r)
  } else if (step.op === "text-node") {
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
  } else if (step.op === "text-add") {
    const target = nodes.get(step.target)
    if (!(target instanceof TextRenderable)) {
      throw new Error(`text-add target ${step.target} is not a TextRenderable`)
    }
    if (step.text === undefined) throw new Error("text-add needs text")
    target.add(step.text)
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
    const r = new ScrollBoxRenderable(renderer, {
      ...boxish,
      ...(Object.keys(trackOpts).length ? { scrollbarOptions: { trackOptions: trackOpts } } : {}),
      ...(o.viewportOptions !== undefined ? { viewportOptions: sub("viewportOptions") } : {}),
      ...(o.contentOptions !== undefined ? { contentOptions: sub("contentOptions") } : {}),
      ...(o.wrapperOptions !== undefined ? { wrapperOptions: sub("wrapperOptions") } : {}),
    })
    // ScrollBox routes child adds into its content viewport (ScrollBox.add).
    parent.add(r)
    nodes.set(step.id, r)
  } else if (step.op === "flush") {
    await setup.flush()
    continue
  } else {
    throw new Error(`unknown op: ${step.op}`)
  }
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
