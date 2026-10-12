// Event-mode oracle: feeds byte sequences to the REAL renderer's
// stdin and records the keypress/keyrelease/paste event stream the
// renderer emits (renderer.keyInput listeners — the InternalKeyHandler
// global side). The Rust port replays the same feed through its
// renderer and must produce the EXACT same event sequence.
//
// Sequences that the StdinParser defers (lone ESC waiting on timeout,
// unparseable prefixes) produce no immediate event; they are recorded
// as `deferred: true` and the Rust side skips them (documented
// follow-up: the full protocol-context state machine).
//
// Usage (from tools/diff/sandbox): bun ../js/event-corpus.ts <out.json>
import { createTestRenderer } from "@opentui/core/testing"
import type { KeyEvent, PasteEvent } from "@opentui/core"

const outPath = process.argv[2] ?? "../out/char/event-corpus.json"

function keyEventLog(evt: KeyEvent) {
  return {
    kind: evt.eventType === "release" ? "keyrelease" : "keypress",
    name: evt.name,
    ctrl: evt.ctrl,
    meta: evt.meta,
    shift: evt.shift,
    option: evt.option,
    number: evt.number,
    sequence: Array.from(evt.sequence).map((c) => c.codePointAt(0)!),
    raw: Array.from(evt.raw).map((c) => c.codePointAt(0)!),
    eventType: evt.eventType,
    source: evt.source,
    code: evt.code ?? null,
    super: evt.super ?? null,
    hyper: evt.hyper ?? null,
    capsLock: evt.capsLock ?? null,
    numLock: evt.numLock ?? null,
    baseCode: evt.baseCode ?? null,
    repeated: evt.repeated ?? null,
  }
}

function pasteEventLog(evt: PasteEvent) {
  return { kind: "paste", bytes: Array.from(evt.bytes) }
}

async function runProfile(kitty: boolean) {
  // Each case runs on a FRESH renderer: the StdinParser is stateful
  // (an incomplete unit stays pending and would absorb the next
  // case's bytes), so independent cases need independent feeds.
  const events: any[] = []
  let renderer: any
  const boot = async () => {
    const setup = await createTestRenderer({
      width: 40,
      height: 10,
      kittyKeyboard: kitty ? { events: true } : undefined,
    })
    renderer = setup.renderer
    renderer.keyInput.on("keypress", (evt: KeyEvent) => events.push(keyEventLog(evt)))
    renderer.keyInput.on("keyrelease", (evt: KeyEvent) => events.push(keyEventLog(evt)))
    renderer.keyInput.on("paste", (evt: PasteEvent) => events.push(pasteEventLog(evt)))
  }
  const feed = (bytes: number[]) => {
    renderer.stdin.emit("data", Buffer.from(Uint8Array.from(bytes)))
  }

  const cases: { label: string; chunks: number[][] }[] = []

  // --- complete key units (immediate events) ---
  const singles: number[][] = []
  for (let b = 1; b < 128; b++) singles.push([b])
  for (const seq of [
    [27, 13], [27, 10], [27, 8], [27, 127], [27, 32], // meta specials
    [27, 91, 65], [27, 91, 66], [27, 91, 67], [27, 91, 68], // arrows
    [27, 91, 72], [27, 91, 70], [27, 91, 51, 126], // home end delete
    [27, 91, 49, 59, 50, 65], [27, 91, 51, 59, 53, 126], // shift+up ctrl+delete
    [27, 91, 90], // shift+tab
    [27, 79, 80], [27, 79, 81], // f1 f2
    [27, 91, 49, 53, 126], [27, 91, 49, 55, 126], // f5 f6
    [27, 79, 112], [27, 79, 116], [27, 79, 77], // SS3 digits + enter
    [27, 91, 91, 65], // cygwin f1
    [27, 27, 91, 65], // double-esc arrow
    [27, 91, 50, 48, 48, 126], // paste start marker (filtered → no key)
    [27, 91, 52, 59, 49, 48, 59, 50, 48, 116], // resize report (no key)
    [27, 91, 49, 48, 59, 50, 48, 82], // CPR (no key)
    [27, 91, 63, 54, 50, 59, 99], // DA (no key)
  ]) singles.push(seq)
  // ctrl+letters and high-byte meta
  for (const b of [1, 3, 26, 28, 29, 30, 31, 127]) singles.push([b])
  // unicode text (UTF-8 BYTES on the wire)
  for (const ch of ["a", "Z", "9", "é", "中", "😀", " "]) singles.push(Array.from(Buffer.from(ch)))

  for (const bytes of singles) {
    cases.push({ label: "k:" + bytes.join(","), chunks: [bytes] })
  }

  // --- kitty CSI-u (only in the kitty profile do these parse as keys) ---
  if (kitty) {
    for (const seq of [
      [27, 91, 57, 55, 117], // a
      [27, 91, 57, 55, 59, 53, 117], // ctrl+a
      [27, 91, 51, 50, 59, 50, 117], // shift+space
      [27, 91, 49, 50, 56, 53, 49, 50, 117], // emoji
      [27, 91, 53, 55, 51, 53, 50, 117], // up
      [27, 91, 49, 59, 49, 58, 49, 65], // up press event
      [27, 91, 57, 55, 59, 49, 58, 51, 117], // a release
      [27, 91, 57, 55, 59, 49, 58, 50, 117], // a repeat
      [27, 91, 53, 55, 51, 57, 57, 117], // kp0 (text 0)
      [27, 91, 57, 55, 58, 54, 53, 59, 50, 117], // shift+a with shifted cp
    ]) {
      cases.push({ label: "k:" + seq.join(","), chunks: [seq] })
    }
  }

  // --- multi-key chunks (one feed, several units) ---
  // lone high byte → the legacy meta+<byte-128> path
  for (const b of [0xe1, 0xad, 0xff]) cases.push({ label: "high:" + b, chunks: [[b]] })

  cases.push({ label: "multi:hello", chunks: [[104, 101, 108, 108, 111]] })
  cases.push({ label: "multi:arrows", chunks: [[27, 91, 65, 27, 91, 66, 27, 91, 67]] })
  cases.push({
    label: "multi:mixed",
    chunks: [[97, 27, 91, 68, 98, 27, 91, 51, 126, 99]],
  })

  // --- bracketed paste (multi-chunk, split markers) ---
  cases.push({
    label: "paste:simple",
    chunks: [[27, 91, 50, 48, 48, 126], Array.from("pasted text").map((c) => c.codePointAt(0)!), [27, 91, 50, 48, 49, 126]],
  })
  cases.push({
    label: "paste:split-marker",
    chunks: [[27, 91, 50, 48, 48, 126], Array.from("abc").map((c) => c.codePointAt(0)!), [27, 91, 50], [48, 49, 126, 97], []],
  })
  cases.push({
    label: "paste:then-key",
    chunks: [[27, 91, 50, 48, 48, 126], Array.from("xy").map((c) => c.codePointAt(0)!), [27, 91, 50, 48, 49, 126, 122]],
  })

  // --- mouse: must NOT produce key events ---
  cases.push({ label: "mouse:sgr", chunks: [[27, 91, 60, 48, 59, 49, 48, 59, 53, 77]] })

  // --- deferred: lone ESC produces no immediate event ---
  cases.push({ label: "defer:esc", chunks: [[27]] })

  const results: any[] = []
  for (const c of cases) {
    await boot()
    const produced: any[] = []
    for (const chunk of c.chunks) {
      feed(chunk)
      produced.push(...events.splice(0))
    }
    results.push({ label: c.label, kitty, chunks: c.chunks, events: produced })
    renderer.destroy()
  }
  return results
}

const legacy = await runProfile(false)
const kitty = await runProfile(true)
await Bun.write(outPath, JSON.stringify({ legacy, kitty }))
console.log(`event corpus: ${legacy.length} legacy, ${kitty.length} kitty -> ${outPath}`)
