// highlight-corpus.ts — freeze the REAL @opentui/core TreeSitterClient
// highlight output (ONESHOT_HIGHLIGHT path: parse + highlights query +
// injections + getSimpleHighlights) for a corpus of (content, filetype).
//
// The Rust port (crates/opentui-core/src/tree_sitter) must reproduce every
// entry byte-identically — this is the grammar-drift measurement instrument
// (bun's shipped .wasm parsers vs the vendored C grammars we compile in).
//
// Usage: bun ../js/highlight-corpus.ts ../../crates/opentui-core/tests/data/highlight-corpus.json
import { TreeSitterClient } from "@opentui/core"

const cases: Array<{ name: string; filetype: string; content: string }> = [
  {
    name: "md-headings",
    filetype: "markdown",
    content: `# H1 heading\n\n## H2 heading\n\n### H3\n\n#### H4\n\n##### H5\n\n###### H6\n`,
  },
  {
    name: "md-setext-heading",
    filetype: "markdown",
    content: `Setext Title\n============\n\nAnother\n---\n`,
  },
  {
    name: "md-inline-emphasis",
    filetype: "markdown",
    content: `plain *italic* and **bold** and ***both*** plus ~~strike~~ and \`code span\` here\n`,
  },
  {
    name: "md-links",
    filetype: "markdown",
    content: `see [the docs](https://example.com/docs) and <https://bare.example.com> plus ![img](pic.png "title")\n`,
  },
  {
    name: "md-lists",
    filetype: "markdown",
    content: `- bullet one\n- bullet two\n\n1. ordered one\n2. ordered two\n\n- [ ] todo\n- [x] done\n`,
  },
  {
    name: "md-quote",
    filetype: "markdown",
    content: `> quoted text line\n> more quote\n`,
  },
  {
    name: "md-thematic-break",
    filetype: "markdown",
    content: `above\n\n---\n\nbelow\n`,
  },
  {
    name: "md-fence-ts",
    filetype: "markdown",
    content: "before\n\n```ts\nconst x: number = 42\nfunction f<T>(a: T): T { return a }\n```\n\nafter\n",
  },
  {
    name: "md-fence-js",
    filetype: "markdown",
    content: "```js\nconst y = { a: 1, b: 'two' }\n// comment\n```\n",
  },
  {
    name: "md-fence-zig",
    filetype: "markdown",
    content: "```zig\nconst std = @import(\"std\");\npub fn main() !void {}\n```\n",
  },
  {
    name: "md-fence-unknown-lang",
    filetype: "markdown",
    content: "```rust\nfn main() { let x = 1; }\n```\n",
  },
  {
    name: "md-fence-no-lang",
    filetype: "markdown",
    content: "```\nplain fenced\n```\n",
  },
  {
    name: "md-fence-open-ended",
    filetype: "markdown",
    content: "```ts\nconst unterminated = 1\n```",
  },
  {
    name: "md-table",
    filetype: "markdown",
    content: `| Alpha | Beta |\n| ------: | :--- |\n| one | two |\n| three | four |\n`,
  },
  {
    name: "md-task-list-and-inline-mix",
    filetype: "markdown",
    content: `- [ ] task with *emph* and \`code\`\n- [x] done task with [link](https://x.io)\n`,
  },
  {
    name: "md-unicode",
    filetype: "markdown",
    content: `# 中文标题\n\nbody with 🙂 emoji, **粗体** and \`代码\` spans\n`,
  },
  {
    name: "md-indented-code",
    filetype: "markdown",
    content: `text\n\n    indented code line\n    another line\n`,
  },
  {
    name: "md-html-and-metadata",
    filetype: "markdown",
    content: `---\ntitle: front matter\n---\n\n<div>html block</div>\n`,
  },
  { name: "md-empty", filetype: "markdown", content: "" },
  { name: "md-plain", filetype: "markdown", content: "just a paragraph\nsecond line wraps\n" },
  { name: "js-basics", filetype: "javascript", content: "const a = 1\nlet s = `tpl ${a}`\nfunction f(x) { return x ?? 0 }\n// line comment\n" },
  { name: "jsx-sample", filetype: "javascriptreact", content: "const el = <div className=\"x\">hi</div>\n" },
  { name: "ts-basics", filetype: "typescript", content: "interface I { a: number }\nconst f = async (x: I): Promise<number> => x.a\n" },
  { name: "tsx-sample", filetype: "typescriptreact", content: "const c = () => <Comp prop={1} />\n" },
  { name: "zig-basics", filetype: "zig", content: "const std = @import(\"std\");\npub fn main() !void {\n    std.debug.print(\"hi\\n\", .{});\n}\n" },
  { name: "unknown-filetype", filetype: "cobol", content: "MOVE 1 TO X.\n" },
  { name: "md-inline-filetype", filetype: "markdown_inline", content: "plain *emph* and [link](https://x.io)\n" },
]

const client = new TreeSitterClient({ dataPath: "/tmp/opencode/highlight-corpus-data" })
client.on("error", (e: string) => console.log("ERR:", e))
client.on("worker:log", (t: string, d: string) => console.log("LOG:", t, d))

await client.initialize()

const out: Array<{ name: string; filetype: string; content: string; highlights: unknown }> = []
for (const c of cases) {
  const result = await client.highlightOnce(c.content, c.filetype)
  if (result.error) throw new Error(`${c.name}: ${result.error}`)
  const rawHighlights = result.highlights ?? []
  out.push({
    name: c.name,
    filetype: c.filetype,
    content: c.content,
    // meta objects only when non-empty (worker output shape)
    highlights: rawHighlights.map((h: any[]) => {
      if (h[3] && Object.keys(h[3] as object).length > 0) return h
      return [h[0], h[1], h[2]]
    }),
  })
  console.error(`ok: ${c.name} -> ${rawHighlights.length} highlights`)
}
await client.destroy()

const outPath = process.argv[2]
if (!outPath) {
  console.error("usage: bun ../js/highlight-corpus.ts <out.json>")
  process.exit(2)
}
await Bun.write(outPath, JSON.stringify(out, null, 1))
console.log(`froze ${out.length} cases -> ${outPath}`)
process.exit(0)
