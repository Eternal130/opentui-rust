// render-traversal-oracle.ts — the REAL @opentui 0.5.14 frame numbers.
//
// Adapted from upstream's own benchmark
// (packages/core/src/benchmark/render-traversal-benchmark.ts @ v0.5.14):
// the same scenario trees (buildOpencodeLayoutTree, the culling-scaling
// scrollbox, the stats harness — avg/median/p95/min/max via
// performance.now per iteration) with two port-lane additions
// (word_wrap_paragraphs, deep_box_chain) mirrored 1:1 in
// tools/bench/src/lib.rs so the Rust rig's numbers compare against the
// real package driven under bun ON THE SAME BOX.
//
// Oracle discipline (see tools/diff/README "the resolution incident"):
//  * imports resolve through oracle/node_modules -> ../../diff/sandbox
//    (the pinned bun.lock install) — never a global cache;
//  * the resolved @opentui/core version is ASSERTED to be exactly
//    0.5.14 — a wrong version fails before any timing runs.
//
// Usage: bun render-traversal-oracle.ts [iterations] [warmup] [out.json]
// (run through oracle/run-oracle.sh, which wires resolution)

import { performance } from "node:perf_hooks"
import { createRequire } from "node:module"
import { mkdir, writeFile } from "node:fs/promises"
import path from "node:path"
import { BoxRenderable, RGBA, ScrollBoxRenderable, TextRenderable } from "@opentui/core"
import { createTestRenderer, type TestRenderer } from "@opentui/core/testing"

// --- version assertion: only 0.5.14 may produce oracle numbers ------------
const require = createRequire(import.meta.url)
const corePkg = require("@opentui/core/package.json") as { version: string }
if (corePkg.version !== "0.5.14") {
  console.error(`oracle discipline: resolved @opentui/core@${corePkg.version}, expected 0.5.14 — refusing to benchmark`)
  process.exit(1)
}

type ScenarioRuntime = {
  renderablesPerIteration: number
  runIteration: (iteration: number) => Promise<void>
  teardown?: () => void
}

type ScenarioResult = {
  name: string
  iterations: number
  warmupIterations: number
  avgMs: number
  medianMs: number
  p95Ms: number
  minMs: number
  maxMs: number
  renderablesPerIteration: number
}

const COLORS = {
  transparent: RGBA.fromInts(0, 0, 0, 0),
  panel: RGBA.fromInts(28, 32, 38),
  element: RGBA.fromInts(40, 46, 56),
  menu: RGBA.fromInts(35, 40, 48),
} as const

const iterations = Math.max(1, Number(process.argv[2] ?? 300))
const warmupIterations = Math.max(0, Number(process.argv[3] ?? 40))
const outArg = process.argv[4]
const SIZES: Array<[number, number]> = [[80, 24], [120, 30], [200, 50]]

function clearRoot(renderer: TestRenderer): void {
  for (const child of renderer.root.getChildren()) {
    child.destroyRecursively()
  }
}

// buildOpencodeLayoutTree — verbatim structure from upstream's benchmark
async function buildOpencodeLayoutTree(
  renderer: TestRenderer,
  renderOnce: () => Promise<void>,
  width: number,
  height: number,
  includeVisibleBoxes: boolean,
  includeText: boolean,
): Promise<{ root: ReturnType<typeof BoxRenderable.prototype.constructor>; renderables: number }> {
  let renderables = 0
  const root = new BoxRenderable(renderer, {
    id: "bench-root",
    width: "100%",
    height: "100%",
    border: false,
    backgroundColor: COLORS.transparent,
    flexDirection: "column",
  })
  renderables += 1
  renderer.root.add(root)

  const messageCount = includeVisibleBoxes ? Math.max(40, height + 8) : Math.max(48, height + 12)

  const header = new BoxRenderable(renderer, {
    id: "bench-header",
    width: "100%",
    height: 3,
    flexDirection: "row",
    paddingLeft: 1,
    paddingRight: 1,
    gap: 1,
  })
  renderables += 1
  root.add(header)
  for (let i = 0; i < 5; i++) {
    header.add(
      new BoxRenderable(renderer, {
        id: `bench-chip-${i}`,
        flexShrink: 0,
        paddingLeft: 1,
        paddingRight: 1,
      }),
    )
    renderables += 1
  }

  const body = new BoxRenderable(renderer, {
    id: "bench-body",
    width: "100%",
    flexGrow: 1,
    flexDirection: "row",
    gap: 1,
    paddingLeft: 1,
    paddingRight: 1,
  })
  renderables += 1
  root.add(body)

  const sidebar = new BoxRenderable(renderer, {
    id: "bench-sidebar",
    width: 22,
    minWidth: 22,
    maxWidth: 22,
    flexShrink: 0,
    flexDirection: "column",
    gap: 1,
  })
  renderables += 1
  body.add(sidebar)
  for (let i = 0; i < 12; i++) {
    sidebar.add(
      new BoxRenderable(renderer, {
        id: `bench-sidebar-row-${i}`,
        height: 1,
        paddingLeft: 1,
        paddingRight: 1,
      }),
    )
    renderables += 1
  }

  const main = new BoxRenderable(renderer, {
    id: "bench-main",
    flexGrow: 1,
    flexDirection: "column",
    gap: 1,
  })
  renderables += 1
  body.add(main)

  for (let i = 0; i < messageCount; i++) {
    const row = new BoxRenderable(renderer, {
      id: `bench-row-${i}`,
      width: "100%",
      flexDirection: "row",
    })
    const rail = new BoxRenderable(renderer, {
      id: `bench-rail-${i}`,
      width: 3,
      minWidth: 3,
      maxWidth: 3,
      flexShrink: 0,
    })
    const content = new BoxRenderable(renderer, {
      id: `bench-content-${i}`,
      flexGrow: 1,
      flexDirection: "column",
    })
    const meta = new BoxRenderable(renderer, {
      id: `bench-meta-${i}`,
      width: "100%",
      height: 1,
      flexDirection: "row",
      justifyContent: "space-between",
    })
    const badges = new BoxRenderable(renderer, {
      id: `bench-badges-${i}`,
      flexDirection: "row",
      gap: 1,
    })
    const actions = new BoxRenderable(renderer, {
      id: `bench-actions-${i}`,
      flexDirection: "row",
      gap: 1,
      flexShrink: 0,
    })
    renderables += 6
    meta.add(badges)
    meta.add(actions)
    content.add(meta)

    if (includeVisibleBoxes) {
      content.add(
        new BoxRenderable(renderer, {
          id: `bench-leaf-${i}`,
          width: "100%",
          height: i % 5 === 0 ? 3 : i % 2 === 0 ? 2 : 1,
          border: false,
          backgroundColor: i % 3 === 0 ? COLORS.menu : i % 2 === 0 ? COLORS.panel : COLORS.element,
        }),
      )
      renderables += 1
    }
    if (includeText && i % 4 === 0) {
      content.add(new TextRenderable(renderer, { id: `bench-text-${i}`, content: `message-${i}` }))
      renderables += 1
    }

    row.add(rail)
    row.add(content)
    main.add(row)
  }

  const footer = new BoxRenderable(renderer, {
    id: "bench-footer",
    width: "100%",
    height: 4,
    flexDirection: "row",
    gap: 1,
    paddingLeft: 1,
    paddingRight: 1,
  })
  renderables += 1
  root.add(footer)
  for (let i = 0; i < 6; i++) {
    footer.add(
      new BoxRenderable(renderer, {
        id: `bench-footer-item-${i}`,
        flexDirection: "row",
        gap: 1,
      }),
    )
    renderables += 1
  }

  await renderOnce()
  return { root, renderables }
}

const SENTENCE =
  "wraps at the viewport width with word wrap, measuring the widest wrapped line and painting every virtual line through the cluster sweep"

async function scenarios(): Promise<Array<(ctx: { renderer: TestRenderer; renderOnce: () => Promise<void>; width: number; height: number }) => Promise<{ name: string; runtime: ScenarioRuntime }>>> {
  return [
    async (ctx) => ({
      name: `layout_only_opencode_wrappers/${ctx.width}x${ctx.height}`,
      runtime: await (async () => {
        const state = await buildOpencodeLayoutTree(ctx.renderer, ctx.renderOnce, ctx.width, ctx.height, false, false)
        return {
          renderablesPerIteration: state.renderables,
          runIteration: async () => {
            await ctx.renderOnce()
          },
          teardown: () => state.root.destroyRecursively(),
        }
      })(),
    }),
    async (ctx) => ({
      name: `mixed_opencode_wrappers/${ctx.width}x${ctx.height}`,
      runtime: await (async () => {
        const state = await buildOpencodeLayoutTree(ctx.renderer, ctx.renderOnce, ctx.width, ctx.height, true, true)
        return {
          renderablesPerIteration: state.renderables,
          runIteration: async () => {
            await ctx.renderOnce()
          },
          teardown: () => state.root.destroyRecursively(),
        }
      })(),
    }),
    async (ctx) => ({
      name: `scrollbox_culling_scaling/${ctx.width}x${ctx.height}`,
      runtime: await (async () => {
        clearRoot(ctx.renderer)
        const childCount = Math.max(120, ctx.height * 8)
        const root = new BoxRenderable(ctx.renderer, {
          id: "bench-cull-root",
          width: "100%",
          height: "100%",
          border: false,
          backgroundColor: COLORS.transparent,
        })
        ctx.renderer.root.add(root)
        const scrollBox = new ScrollBoxRenderable(ctx.renderer, {
          id: "bench-sb",
          width: "100%",
          height: "100%",
          viewportCulling: true,
        })
        root.add(scrollBox)
        for (let i = 0; i < childCount; i++) {
          scrollBox.add(
            new BoxRenderable(ctx.renderer, {
              id: `bench-cull-row-${i}`,
              width: "100%",
              height: 1,
              flexShrink: 0,
              border: false,
              backgroundColor: i % 2 === 0 ? COLORS.panel : COLORS.element,
            }),
          )
        }
        await ctx.renderOnce()
        scrollBox.scrollTo(Math.floor(childCount / 2))
        await ctx.renderOnce()
        return {
          renderablesPerIteration: childCount + 2,
          runIteration: async (i) => {
            scrollBox.scrollBy(i % 2 === 0 ? 1 : -1)
            await ctx.renderOnce()
          },
          teardown: () => root.destroyRecursively(),
        }
      })(),
    }),
    async (ctx) => ({
      name: `word_wrap_paragraphs/${ctx.width}x${ctx.height}`,
      runtime: await (async () => {
        clearRoot(ctx.renderer)
        const texts: TextRenderable[] = []
        for (let i = 0; i < Math.max(0, ctx.height - 2); i++) {
          const t = new TextRenderable(ctx.renderer, {
            id: `bench-para-${i}`,
            content: `para ${String(i).padStart(2, "0")}: ${SENTENCE} ${SENTENCE} ${SENTENCE}`,
          })
          ctx.renderer.root.add(t)
          texts.push(t)
        }
        await ctx.renderOnce()
        return {
          renderablesPerIteration: texts.length,
          runIteration: async () => {
            await ctx.renderOnce()
          },
          teardown: () => {
            for (const t of texts) t.destroyRecursively()
          },
        }
      })(),
    }),
    async (ctx) => ({
      name: `deep_box_chain/${ctx.width}x${ctx.height}`,
      runtime: await (async () => {
        clearRoot(ctx.renderer)
        const depth = Math.min(ctx.height, 24)
        let parent: BoxRenderable | undefined
        const chain: BoxRenderable[] = []
        for (let i = 0; i < depth; i++) {
          const box = new BoxRenderable(ctx.renderer, {
            id: `bench-deep-${i}`,
            width: "100%",
            height: Math.max(1, ctx.height - i),
            border: false,
            backgroundColor: COLORS.panel,
            padding: 1,
            overflow: i % 6 === 3 ? "hidden" : undefined,
          })
          ;(parent ?? ctx.renderer.root).add(box)
          chain.push(box)
          parent = box
        }
        parent?.add(
          new TextRenderable(ctx.renderer, {
            id: "bench-deep-leaf",
            content: "deep leaf text ".repeat(Math.floor(ctx.width / 4) + 1),
          }),
        )
        await ctx.renderOnce()
        return {
          renderablesPerIteration: chain.length + 1,
          runIteration: async () => {
            await ctx.renderOnce()
          },
          teardown: () => chain[0]?.destroyRecursively(),
        }
      })(),
    }),
  ]
}

function stats(samples: number[]) {
  const sorted = [...samples].sort((a, b) => a - b)
  const avg = samples.reduce((s, v) => s + v, 0) / samples.length
  const mid = Math.floor(sorted.length / 2)
  return {
    avgMs: avg,
    medianMs: sorted.length % 2 === 0 ? (sorted[mid - 1] + sorted[mid]) / 2 : sorted[mid],
    p95Ms: sorted[Math.floor((sorted.length - 1) * 0.95)],
    minMs: sorted[0],
    maxMs: sorted[sorted.length - 1],
  }
}

async function main() {
  const results: ScenarioResult[] = []
  for (const [width, height] of SIZES) {
    const { renderer, renderOnce } = await createTestRenderer({
      width,
      height,
      targetFps: 60,
      maxFps: 60,
      screenMode: "main-screen",
      externalOutputMode: "passthrough",
      consoleMode: "disabled",
      useMouse: false,
    })
    const ctx = { renderer, renderOnce, width, height }
    for (const make of await scenarios()) {
      const { name, runtime } = await make(ctx)
      for (let i = 0; i < warmupIterations; i++) await runtime.runIteration(i)
      const samples: number[] = []
      for (let i = 0; i < iterations; i++) {
        const start = performance.now()
        await runtime.runIteration(i)
        samples.push(performance.now() - start)
      }
      const s = stats(samples)
      results.push({
        name,
        iterations,
        warmupIterations,
        renderablesPerIteration: runtime.renderablesPerIteration,
        ...s,
      })
      console.log(`${name}: median ${s.medianMs.toFixed(3)} ms (p95 ${s.p95Ms.toFixed(3)} ms, min ${s.minMs.toFixed(3)} ms)`)
      runtime.teardown?.()
      clearRoot(renderer)
    }
    renderer.destroy()
  }

  if (outArg) {
    const out = path.resolve(outArg)
    await mkdir(path.dirname(out), { recursive: true })
    await writeFile(
      out,
      JSON.stringify(
        {
          metadata: {
            runtime: "bun",
            version: process.versions.bun ?? "unknown",
            core: corePkg.version,
            iterations,
            warmupIterations,
            timestamp: new Date().toISOString(),
          },
          scenarios: results,
        },
        null,
        2,
      ),
    )
    console.log(`wrote ${out}`)
  }
}

await main()
