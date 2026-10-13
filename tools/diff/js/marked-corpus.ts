// marked-corpus.ts — freeze the REAL marked@17.0.1 lexer output (the
// exact package @opentui/core@0.5.14 depends on) for a corpus of
// markdown documents. The Rust port (markdown_parser.rs) must reproduce
// every token tree byte-identically (type/raw/text/depth/ordered/start/
// loose/items/header/rows/align/tokens recursion).
//
// Usage: bun ../js/marked-corpus.ts ../../crates/opentui-core/tests/data/marked-corpus.json
import { Lexer } from "marked"

type Json = any

function tokenizeInline(src: string): Json {
  const tokens = Lexer.lexInline(src)
  return tokens.map(simplify)
}

function simplify(token: Json): Json {
  const out: Json = { type: token.type, raw: token.raw }
  if (token.text !== undefined && token.type !== "text") out.text = token.text
  if (token.type === "text" && token.text !== undefined) out.text = token.text
  if (token.depth !== undefined) out.depth = token.depth
  if (token.ordered !== undefined) out.ordered = token.ordered
  if (token.start !== undefined) out.start = token.start
  if (token.loose !== undefined) out.loose = token.loose
  if (token.lang !== undefined) out.lang = token.lang
  if (token.href !== undefined) out.href = token.href
  if (token.title !== undefined) out.title = token.title
  if (token.task !== undefined) out.task = token.task
  if (token.checked !== undefined) out.checked = token.checked
  if (token.align !== undefined) out.align = token.align
  if (token.header !== undefined) out.header = token.header.map((c: Json) => ({ text: c.text, tokens: c.tokens ? c.tokens.map(simplify) : undefined }))
  if (token.rows !== undefined) out.rows = token.rows.map((row: Json[]) => row.map((c) => ({ text: c.text, tokens: c.tokens ? c.tokens.map(simplify) : undefined })))
  if (token.items !== undefined) out.items = token.items.map(simplify)
  if (token.tokens !== undefined) out.tokens = token.tokens.map(simplify)
  return out
}

const cases: Array<{ name: string; content: string }> = [
  { name: "paragraphs", content: "first para\nsecond line\n\nsecond para\n" },
  { name: "atx-headings", content: "# one\n## two\n### three\n#### four\n##### five\n###### six\n####### seven-not-heading\n#no-space-not-heading\n" },
  { name: "heading-closing", content: "# closed ##\n# trailing ###   \n## mid #dle\n" },
  { name: "setext", content: "Title\n=====\n\nSub\n---\n\nmixed\n===\n" },
  { name: "fences", content: "```js\ncode\nlines\n```\n\n~~~\ntilde fence\n~~~\n\n```open ended\nno close\n" },
  { name: "fence-indent", content: "  ```\n  indented\n  ```\n\n   ```\n   deep\n``` closing at zero\n" },
  { name: "indented-code", content: "    indented code\n    more\n\ntext after\n" },
  { name: "hr", content: "above\n\n---\n\n***\n\n___\n\n- - -\n\n *** \n" },
  { name: "hr-not", content: "--\n**\n_ _\n" },
  { name: "blockquote", content: "> quoted\n> more\n\n> lazy\ncontinuation\n" },
  { name: "blockquote-nested", content: "> outer\n> > inner\n" },
  { name: "ul-tight", content: "- one\n- two\n- three\n" },
  { name: "ul-loose", content: "- one\n\n- two\n" },
  { name: "ol", content: "1. first\n2. second\n" },
  { name: "ol-paren", content: "1) first\n2) second\n" },
  { name: "ol-start", content: "5. five\n6. six\n" },
  { name: "nested-list", content: "- outer\n  - inner one\n  - inner two\n- back\n" },
  { name: "list-code", content: "- item\n\n  ```\n  fenced in list\n  ```\n" },
  { name: "list-para", content: "- item one\n\n  continued para\n- item two\n" },
  { name: "task-list", content: "- [ ] todo\n- [x] done\n- plain\n" },
  { name: "list-marker-mix", content: "* star\n\n+ plus\n" },
  { name: "table-basic", content: "| a | b |\n| - | - |\n| 1 | 2 |\n" },
  { name: "table-align", content: "| left | mid | right |\n| :--- | :---: | ---: |\n| 1 | 2 | 3 |\n" },
  { name: "table-inline", content: "| **bold** | `code` | [l](u) |\n| - | - | - |\n| *em* | ~~del~~ | plain |\n" },
  { name: "table-no-leading-pipe", content: "a | b\n- | -\n1 | 2\n" },
  { name: "table-escaped-pipe", content: "| a \\| b | c |\n| - | - |\n| x | y |\n" },
  { name: "table-ragged", content: "| a | b | c |\n| - | - |\n| 1 |\n| 1 | 2 | 3 | 4 |\n" },
  { name: "links", content: "see [docs](https://x.io) and [titled](https://x.io \"the title\") and <https://auto.link> plus ![img](pic.png)\n" },
  { name: "emphasis-basic", content: "plain *em* and **strong** and ***both*** and _u_ and __uu__ plus ~~strike~~\n" },
  { name: "emphasis-nested", content: "**bold *nested em* end** and *outer **inner** tail*\n" },
  { name: "emphasis-intraword", content: "foo*bar*baz and foo_bar_baz\n" },
  { name: "codespan", content: "a `code` b ``double ` tick`` c\n" },
  { name: "escapes", content: "\\*not em\\* and \\[brackets\\] and \\\\ backslash\n" },
  { name: "hard-break", content: "line one  \nline two\\\nline three\n" },
  { name: "html-block", content: "<div>\nblock html\n</div>\n\ntext\n" },
  { name: "def-links", content: "[ref]: https://example.com \"Ref Title\"\n\ntext with [ref] link\n" },
  { name: "mixed-doc", content: "# Title\n\nintro *para* with [link](https://x.io)\n\n- item one\n- item two with `code`\n\n> quote block\n\n```ts\nconst x = 1\n```\n\n| h |\n| - |\n| c |\n\n---\n\nfinal para\n" },
  { name: "unicode-doc", content: "# 中文标题\n\n正文 with 🙂 emoji\n" },
  { name: "blank-line-variants", content: "para one\n\n\n\npara two after many blanks\n" },
  { name: "trailing-newline-none", content: "no trailing newline" },
]

const out: Json[] = []
for (const c of cases) {
  let tokens: Json
  let error: string | undefined
  try {
    tokens = Lexer.lex(c.content, { gfm: true }).map(simplify)
  } catch (e) {
    error = String(e)
    tokens = []
  }
  out.push({ name: c.name, content: c.content, tokens, error })
  console.error(`ok: ${c.name} -> ${Array.isArray(tokens) ? tokens.length : "?"} tokens`)
}

const outPath = process.argv[2]
if (!outPath) {
  console.error("usage: bun ../js/marked-corpus.ts <out.json>")
  process.exit(2)
}
await Bun.write(outPath, JSON.stringify(out, null, 1))
console.log(`froze ${out.length} cases -> ${outPath}`)
process.exit(0)
