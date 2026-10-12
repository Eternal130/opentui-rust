// Characterization corpus generator — runs the REAL @opentui 0.5.14
// parsers over a large generated corpus and dumps (input → output)
// pairs as JSON for the Rust port's replay tests.
//
// Usage: bun js/char-corpust.ts <out.json>
import { parseKeypress, MouseParser, decodePasteBytes } from "@opentui/core"

const outPath = process.argv[2]
if (!outPath) {
  console.error("usage: bun js/char-corpus.ts <out.json>")
  process.exit(2)
}

// ---- corpus construction -------------------------------------------------

const keySequences: string[] = []

// every single byte
for (let b = 0; b < 256; b++) keySequences.push(String.fromCharCode(b))
// ESC + every byte (raw + high)
for (let b = 0; b < 256; b++) keySequences.push("\x1b" + String.fromCharCode(b))
// double ESC forms
keySequences.push("\x1b\x1b", "\x1b\x1b[A", "\x1b\x1bOA", "\x1b\x1b[1;2A")
// control bytes + ESC-prefixed control bytes
for (let b = 0; b < 32; b++) keySequences.push("\x1b" + String.fromCharCode(b))
// SS3 letters
for (const c of "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz") keySequences.push("\x1bO" + c)
for (const c of "MN") keySequences.push("\x1bN" + c)
// CSI letter finals
for (const c of "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz") keySequences.push("\x1b[" + c)
// CSI number ~ (tilde codes incl. rxvt/putty ranges)
for (const n of [1, 2, 3, 4, 5, 6, 7, 8, 11, 12, 13, 14, 15, 17, 18, 19, 20, 21, 23, 24, 25, 29, 31, 32, 33, 34, 36, 57427]) {
  for (const m of [1, 2, 3, 4, 5, 6, 7, 8, 9, 16, 17]) {
    keySequences.push(`\x1b[${n}~`)
    keySequences.push(`\x1b[${n};${m}~`)
    keySequences.push(`\x1b[${n};${m}:1~`)
    keySequences.push(`\x1b[${n};${m}:2~`)
    keySequences.push(`\x1b[${n};${m}:3~`)
    keySequences.push(`\x1b[${n};1;${m}~`)
  }
}
// CSI 1;m letter (arrow keys with modifiers, both wire shapes)
for (const letter of "ABCDEFGHNPQS") {
  for (const m of [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 16, 17, 33]) {
    keySequences.push(`\x1b[1;${m}${letter}`)
    keySequences.push(`\x1b[1;${m}:1${letter}`)
    keySequences.push(`\x1b[1;${m}:2${letter}`)
    keySequences.push(`\x1b[1;${m}:3${letter}`)
    keySequences.push(`\x1b[${letter}`)
  }
}
// rxvt-style modifier letters and $ / ^ terminators
for (const code of ["[a", "[b", "[c", "[d", "[e", "[2$", "[3$", "[5$", "[6$", "[7$", "[8$", "[2^", "[3^", "[5^", "[6^", "[7^", "[8^", "[Z", "[[A", "[[B", "[[C", "[[D", "[[E", "[[5~", "[[6~", "Oa", "Ob", "Oc", "Od", "Oe"]) {
  keySequences.push("\x1b" + code)
}
// modifyOtherKeys
for (const mod of [1, 2, 3, 4, 5, 6, 7, 8, 9, 17]) {
  for (const code of [8, 9, 13, 27, 32, 48, 53, 65, 97, 127, 128, 233, 128512]) {
    keySequences.push(`\x1b[27;${mod};${code}~`)
  }
}
// kitty CSI-u forms
const kittyCodepoints = [
  0, 1, 9, 13, 27, 32, 48, 65, 97, 122, 127, 128, 233, 0x4E00, 0xAC00, 128512, 57344, 57347, 57348,
  57350, 57352, 57354, 57356, 57357, 57364, 57375, 57399, 57409, 57413, 57414, 57415, 57427,
  57428, 57441, 57452, 57453, 57454, 1114111, 1114112, // NOTE: surrogate codepoints (0xD800-0xDFFF)
  // are excluded — upstream String.fromCodePoint accepts them and the
  // parsed key carries a lone-surrogate name, which Rust strings cannot
  // represent (documented divergence in the kitty parser)
]
const kittyModifiers = [1, 2, 3, 4, 5, 6, 7, 8, 9, 16, 17, 32, 33, 64, 65, 129]
for (const cp of kittyCodepoints) {
  keySequences.push(`\x1b[${cp}u`)
  for (const m of kittyModifiers) {
    keySequences.push(`\x1b[${cp};${m}u`)
    for (const e of ["", ":1", ":2", ":3", ":9"]) {
      keySequences.push(`\x1b[${cp};${m}${e}u`)
    }
  }
  // shifted/base-layout variants
  keySequences.push(`\x1b[${cp}:65;2u`)
  keySequences.push(`\x1b[${cp}:65:97;2u`)
  keySequences.push(`\x1b[${cp}::99;5u`)
  // text field
  keySequences.push(`\x1b[${cp};;${cp}u`)
}
// mouse / response / paste sequences that must filter to null
keySequences.push(
  "\x1b[<0;1;1M", "\x1b[<0;1;1m", "\x1b[<64;10;5M", "[<0;1;1M", "\x1b[<0;1;", "\x1b[<", "[<32;",
  "\x1b[M !!", "\x1b[4;10;20t", "\x1b[8;10;20t", "\x1b[10;20R", "\x1b[?62;c", "\x1b[?1;2$y",
  "\x1b[I", "\x1b[O", "\x1b]0;title\x07", "\x1b]11;rgb:1/1/1\x1b\\", "\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\",
  "\x1b[200~", "\x1b[201~", "\x1b[99;1u", "\x1b[u", "\x1b[;2u", "\x1b[97;;u",
)
// unicode singles
for (const ch of ["é", "中", "😀", "ㅊ", "в", " ", "\u{FEFF}", "\u{200B}"]) keySequences.push(ch)
// strings from the upstream test files (extracted literals below)
for (const s of upstreamTestLiterals()) keySequences.push(s)

function upstreamTestLiterals(): string[] {
  // A curated set of literals from parse.keypress*.test.ts that the
  // generated grids above do not already cover.
  return [
    "\x1b[200~", "\x1b[201~", "\x1b[27;2;13~", "\x1b[27;5;13~", "\x1b[27;5;27~", "\x1b[27;5;9~",
    "\x1b[27;2;32~", "\x1b[27;2;127~", "\x1b[27;2;8~", "\x1b[27;3;53~", "\x1b[27;1;97~",
    "\x1b[57399u", "\x1b[97;1:2u", "\x1b[97;5:3u", "\x1b[1;2:2A", "\x1b[1;5:3B", "\x1b[5;1:2~",
    "\x1b[3;5:3~", "\x1b[97;1:9u", "\x1b[97;1:u", "\x1b[1114112u", "\x1b[233u", "\x1b[128512u",
    "\x1b[97;65u", "\x1b[97;129u", "\x1b[57414u", "\x1b[99;2u", "\x1b[1074;2u",
  ]
}

// ---- run the oracle ------------------------------------------------------

function parsedToJSON(p: any) {
  if (p === null || p === undefined) return null
  return {
    name: p.name,
    ctrl: p.ctrl,
    meta: p.meta,
    shift: p.shift,
    option: p.option,
    sequence: Array.from(p.sequence).map((c: string) => c.codePointAt(0)!),
    number: p.number,
    raw: Array.from(p.raw).map((c: string) => c.codePointAt(0)!),
    eventType: p.eventType,
    source: p.source,
    code: p.code ?? null,
    super: p.super ?? null,
    hyper: p.hyper ?? null,
    capsLock: p.capsLock ?? null,
    numLock: p.numLock ?? null,
    baseCode: p.baseCode ?? null,
    repeated: p.repeated ?? null,
  }
}

const corpus: any[] = []
const seen = new Set<string>()
for (const seq of keySequences) {
  if (seen.has(seq)) continue
  seen.add(seq)
  corpus.push({
    input: Array.from(seq).map((c) => c.codePointAt(0)!),
    legacy: parsedToJSON(parseKeypress(seq, {})),
    kitty: parsedToJSON(parseKeypress(seq, { useKittyKeyboard: true })),
  })
}

// ---- mouse corpus ---------------------------------------------------------

const mouseSequences: string[] = []
for (const btn of [0, 1, 2, 3, 4, 5, 32, 33, 34, 35, 36, 64, 65, 66, 67, 96, 97, 128]) {
  for (const term of ["M", "m"]) {
    for (const [x, y] of [[1, 1], [10, 5], [100, 50], [0, 0]]) {
      mouseSequences.push(`\x1b[<${btn};${x};${y}${term}`)
    }
  }
}
// X10: ESC [ M + button(32+code) + x(33+col) + y(33+row) incl. high bytes
for (const code of [0, 1, 2, 3, 32, 33, 64, 65, 96]) {
  mouseSequences.push("\x1b[M" + String.fromCharCode(32 + code) + String.fromCharCode(33 + 9) + String.fromCharCode(33 + 4))
}
mouseSequences.push("\x1b[M" + String.fromCharCode(32 + 0) + String.fromCharCode(255) + String.fromCharCode(254))
// runs + non-mouse + malformed
mouseSequences.push("\x1b[<0;1;1M\x1b[<0;1;1m", "\x1b[<64;1;1M\x1b[<65;1;1M\x1b[<66;1;1M", "abc", "\x1b[A", "\x1b[<0;1M", "\x1b[<0;1;1X", "\x1b[M !")

function mouseToJSON(e: any) {
  if (e === null || e === undefined) return null
  return {
    type: e.type,
    button: e.button,
    x: e.x,
    y: e.y,
    modifiers: e.modifiers,
    scroll: e.scroll ?? null,
  }
}

const mouseCorpus: any[] = []
for (const seq of mouseSequences) {
  // fresh parser per sequence (button-pressed state is order-sensitive)
  const parser = new MouseParser()
  mouseCorpus.push({
    input: Array.from(seq).map((c) => c.codePointAt(0)!),
    single: mouseToJSON(parser.parseMouseEvent(Buffer.from(seq, "latin1"))),
    all: parser.parseAllMouseEvents(Buffer.from(seq, "latin1")).map(mouseToJSON),
  })
}
// stateful drag sequence
{
  const parser = new MouseParser()
  const seqs = ["\x1b[<0;5;5M", "\x1b[<32;6;6M", "\x1b[<32;7;7M", "\x1b[<0;7;7m", "\x1b[<32;8;8M"]
  mouseCorpus.push({
    input: seqs.map((s) => Array.from(Buffer.from(s, "latin1")).map((b) => b)),
    stateful: seqs.map((s) => mouseToJSON(parser.parseMouseEvent(Buffer.from(s, "latin1")))),
  })
}

// ---- paste corpus ---------------------------------------------------------

const pasteCorpus: any[] = []
const pasteByteCases: number[][] = [
  Array.from(Buffer.from("hello world")),
  Array.from(Buffer.from("héllo 😀 中")),
  Array.from(Buffer.concat([Buffer.from("a"), Buffer.from([0xe2, 0x28, 0xa1]), Buffer.from("b")])),
  [0xe2, 0x82],
  [0xe2, 0x82, 0xac],
  [0xe1, 0x80],
  [0x80],
  [0xf5],
  [0xff, 0xfe],
  Array.from(Buffer.concat([Buffer.from("\u{FEFF}bom"), Buffer.from("x")])),
  [0xed, 0xa0, 0x80], // surrogate encoded
  [0xf0, 0x9f, 0x98],
]
for (const bytes of pasteByteCases) {
  pasteCorpus.push({ input: bytes, text: Array.from(decodePasteBytes(Uint8Array.from(bytes))).map((c) => c.codePointAt(0)!) })
}

// ---- dump ----------------------------------------------------------------

await Bun.write(outPath, JSON.stringify({ keys: corpus, mouse: mouseCorpus, paste: pasteCorpus }))
console.log(`corpus: ${corpus.length} keys, ${mouseCorpus.length} mouse, ${pasteCorpus.length} paste -> ${outPath}`)
