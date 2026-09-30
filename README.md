# figma-canvas-mcp

[![CI](https://github.com/swiftkimani/figma-canvas-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/swiftkimani/figma-canvas-mcp/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Read a **live Figma canvas** over a local plugin bridge and reconstruct it as code.
One Rust binary, no Figma API token, no REST rate limit, no Node.js runtime.

Works with **Figma Desktop and Figma in a browser tab**.

```
┌──────────────┐  MCP over stdio   ┌──────────────────────┐
│  AI client   │◄─────────────────►│  figma-canvas-mcp    │
│ (Claude Code)│                   │  (single Rust binary)│
└──────────────┘                   └──────────┬───────────┘
                                              │ ws://127.0.0.1:18765
                                              │ (loopback only)
                                   ┌──────────▼───────────┐
                                   │  ui.html  (iframe)   │  ← owns the socket
                                   │      ▲ postMessage   │
                                   │  code.js (sandbox)   │  ← owns the Figma API
                                   └──────────┬───────────┘
                                              │ Plugin API
                                   ┌──────────▼───────────┐
                                   │   your live document │
                                   └──────────────────────┘
```

## Why not just use the REST API?

| | REST API | This |
|---|---|---|
| Rate limit | 60 req/min per user | none — loopback to your own machine |
| Auth | personal access token | none; it reads the file you have open |
| `getCSSAsync()` | not available | yes, Figma's own CSS per node |
| Variables with names | plan-gated | available on any plan |
| Component variant values | partial | full `componentProperties` |
| Asset export at scale | separate render call | `exportAsync()` direct |
| View-only files | needs file access | works if you can open the file |

## Why is there JavaScript if this is a Rust project?

Because Figma's plugin host exposes `figma.currentPage.selection` and
`getCSSAsync()` **to JavaScript only** — nothing else can reach inside that
sandbox. So `plugin/` is a thin projection: it reads Figma's API and forwards a
flat JSON shape, with no logic of its own.

Everything that decides what a design *means* is Rust: the IR, the auto-layout →
flexbox mapping, token resolution, and code generation — all unit-tested.

**There is no Node.js runtime dependency.** No `npm install`, no `node_modules`, no
build step. Figma loads `code.js` as-is.

## Install

Needs a recent stable Rust (the crate uses edition 2024).

```bash
git clone https://github.com/swiftkimani/figma-canvas-mcp
cd figma-canvas-mcp
cargo build --release
# binary at ./target/release/figma-canvas-mcp
```

Or grab a prebuilt binary for Linux, macOS (Intel and Apple Silicon) or Windows
from [Releases](https://github.com/swiftkimani/figma-canvas-mcp/releases) — each
archive bundles the `plugin/` directory too.

### 1. Register the server with your MCP client

Claude Code:

```bash
claude mcp add figma-canvas -- /absolute/path/to/target/release/figma-canvas-mcp
```

Or by hand, in your client's MCP config:

```json
{
  "mcpServers": {
    "figma-canvas": {
      "command": "/absolute/path/to/target/release/figma-canvas-mcp",
      "args": ["--out-dir", "./figma-out"]
    }
  }
}
```

### 2. Load the plugin in Figma

Figma → **Plugins → Development → Import plugin from manifest…** → pick
`plugin/manifest.json`.

Then, with your file open: **Plugins → Development → Figma Canvas Bridge**. The
panel shows a green dot when it is attached. Leave it open while you work.

It reconnects on its own, so a Figma reload or a browser refresh needs no restart.

## Tools

| Tool | What it gives you |
|---|---|
| `figma_status` | Whether the plugin is attached, Desktop vs browser, file, selection |
| `get_selection` | id / name / type / size of what is selected — cheap orientation |
| `read_scene` | The reconstruction. Cheap outline by default; `detail="full"` for one subtree |
| `get_css` | Figma's own `getCSSAsync()` output per node |
| `get_tokens` | Variables as design tokens, plus a ready `tokens.css` with every mode |
| `get_components` | Which component each instance came from, and its variant values |
| `export_assets` | SVG / PNG / JPG / PDF at any scale, written to disk |
| `get_interactions` | Prototype triggers, actions, and motion — durations, easing, and the screen flow |
| `generate_code` | React + TypeScript, with CSS modules, Tailwind, or inline styles |

## The part that actually matters: layout

Naming a colour is easy. Getting layout right is what separates usable output from
a pile of absolutely-positioned divs. The mapping:

| Figma | CSS |
|---|---|
| `layoutMode: HORIZONTAL` / `VERTICAL` | `display: flex` + `flex-direction` |
| `itemSpacing` | `gap` |
| `counterAxisSpacing` (when wrapping) | `row-gap` / `column-gap` |
| `primaryAxisAlignItems: SPACE_BETWEEN` | `justify-content: space-between` |
| `counterAxisAlignItems: CENTER` | `align-items: center` |
| sizing `FILL` on the **main** axis | `flex: 1 1 0%` |
| sizing `FILL` on the **cross** axis | `align-self: stretch` |
| sizing `HUG` | omitted — the flex default already hugs |
| sizing `FIXED` in a flex parent | explicit size **+ `flex-shrink: 0`** |
| `layoutPositioning: ABSOLUTE` | `position: absolute` + constraint-derived offsets |
| constraint `MAX` / `STRETCH` | pins `right`/`bottom`, or both edges |
| bound variable on a fill | `var(--token-name, #literal)` |

Two rules drive this:

1. **Layout comes from our IR; appearance comes from Figma.** `getCSSAsync()` is
   authoritative for paint, type and effects, but it describes a node *in
   isolation* and emits hard pixel widths. Our IR knows the parent relationship, so
   `FILL` survives as `flex: 1` instead of freezing at `width: 327px`. There is a
   test that asserts exactly this and fails if Figma's CSS ever wins.
2. **A component instance stays a component.** Instances are never walked into.
   They emit `<Button variant="primary" size="md" />` with placement passed through
   `className`, because placement is the caller's decision while paint is the
   component's.

## Choosing what to spend

Every read states its own price and the alternatives, so nobody discovers the
cost after paying it:

```
[151 nodes · ~392 tokens spent · 162 sketch · 392 standard ← this · ≈39868 precise · ≈39868 exhaustive]
```

Four levels, measured on the same 151-node screen:

| `fidelity` | what you get | tokens |
|---|---|---|
| `sketch` | the skeleton: structure, sizing, ids — no words | ~162 |
| `standard` *(default)* | full-depth outline with all content | ~392 |
| `precise` | complete model plus Figma's own CSS per node | ~39,900 |
| `exhaustive` | as `precise`, no node limit; depth still bounded by the transport | ~39,900 |

The numbers in that line are measured, not guessed — the scene is already in hand
when the price is printed, so the other renderings are produced and counted.
Only `precise` and `exhaustive` carry a `≈`, because Figma's per-node CSS was not
fetched and is roughly twice the size of the model it annotates.

`sketch` is a table of contents rather than a shallower tree, which matters: a
three-level design is not made cheaper by capping depth, but it is made much
cheaper by dropping the words. It still keeps every node id and id range, so the
follow-up call can ask for exactly what it needs.

## It reads your codebase, not just the design

Generated code that does not match the project it lands in is rework, not output.
So before generating, the target project is inspected and the output follows its
conventions:

| detected from | affects |
|---|---|
| `package.json` dependencies | React · Preact · Vue · Svelte · Angular · Solid |
| `tsconfig.json`, `typescript` dep | `.tsx` with an interface, or `.jsx` with JSDoc |
| `tailwindcss` dep · `*.module.css` files · emotion/styled-components | how styles are emitted |
| `tsconfig` path aliases | `@/components/Button`, not a guessed `./components/Button` |
| `src/components`, `app/components`, … | where imports point |
| Next.js `app/` directory | `"use client"` on anything with a handler |
| `.prettierrc` / `prettier` key | quote style and semicolons |

Point it at your repo with `--project-root`, or let it default to the working
directory. Every conclusion carries the evidence that produced it:

```
Detected: React · TypeScript · Tailwind · alias @/ · app router
  package.json found
  next in dependencies
  tailwindcss in dependencies
  tsconfig path alias @/* -> ./src/*
  Next.js app/ directory: client components need "use client"
```

A detector that cannot say *why* it decided something is impossible to correct,
and when nothing is found it says so rather than pretending: *"No project
detected; emitting React + TypeScript + CSS modules by default."*

### Then a language server checks that guess

File-scanning tells you what a project's *conventions* are. It cannot tell you
whether `Button` actually exists at `@/components/Button`, or whether the design
is referencing a component nobody has built yet. A language server already knows
— it has resolved the project's modules, aliases and types — so it is asked:

```
Detected: React · TypeScript · Tailwind · alias @/ · app router
Verified against the codebase: Button, Badge.
  corrected Badge: @/components/Badge -> @/design-system/Badge
Not found in the codebase: PriceTag. These imports are guesses — the component
may need building first.
```

That last line is the useful one. It is the difference between code that compiles
and code that looks like it should.

Servers are found in `node_modules/.bin` first — the version the project actually
pins — then on `PATH`: `typescript-language-server`, `vtsls`,
`vue-language-server`, `svelteserver`, `ngserver`, picked to match the detected
framework.

**It is enrichment, never a requirement.** If no server is installed, or it fails
to start, or it hangs, generation proceeds on the file-scanned stack and says so.
A tool that breaks when an optional dependency is missing is not optional. The
server is started once and reused, because a TypeScript project can take tens of
seconds to index, and a failure is remembered rather than retried on every call.

Two details that matter for correctness:

- **`workspace/symbol` is a fuzzy search.** Asking for `Button` returns
  `ButtonGroup`, `IconButton` and `buttonStyles`. Only an exact name match of a
  definition-like symbol kind is accepted; otherwise you get an import for a
  component that does not exist under the name being written.
- **A dependency's `Button` is not your `Button`.** A match inside
  `node_modules` loses to one in your own source.

## Code that survives review

Output is held to what a developer would have written by hand:

- **Clickable nodes become `<button type="button">`**, not `<div onClick>`. A
  clickable div is unreachable by keyboard and announces nothing; it fails
  accessibility review every time.
- **Icon-only buttons get an `aria-label`** from the layer name, because a
  button with no text announces nothing at all.
- **Landmarks come from layer names** — a layer called "Site header" becomes
  `<header>`, "Main nav" becomes `<nav>`, "Sidebar" becomes `<aside>`.
- **A repeated run becomes a `<ul>` of `<li>`s**, and the `<li>` *replaces* the
  item's own element rather than wrapping it, so the flex relationship the
  design depends on is preserved. Browser list defaults are reset, since the
  design never asked for them.
- **Identical rules are merged** into one grouped selector. Designs reuse the
  same styling across differently-named layers constantly, and a stylesheet that
  repeats itself is slower to parse and harder to maintain.
- **Class names are valid JS identifiers.** css-loader does not camelCase keys by
  default, so a `.some-class` rule would leave `styles.someClass` undefined at
  runtime — a component that renders silently unstyled with no error anywhere.

## Runs everywhere

Pure portable Rust — the crate contains **no `cfg(target_os)`, no `cfg(unix)`, no
`cfg(windows)`**, and paths go through `PathBuf` throughout. CI runs the whole
suite on Linux, macOS and Windows on every push.

Prebuilt binaries are published for six native targets, all built on real
hardware rather than cross-compiled:

| | x86-64 | ARM64 |
|---|---|---|
| Linux | ✓ | ✓ |
| macOS | ✓ (Intel) | ✓ (Apple Silicon) |
| Windows | ✓ | ✓ |

The Figma plugin is plain browser JavaScript, so it is platform-independent by
construction.

## Token cost, and why size stops mattering

Designed for small context windows, because most people are not on a large one.
A free-tier model, OpenCode, or a local model has a window this tool could
exhaust in a single careless call — so it does not make careless calls.

Measured on an ordinary 151-node screen (a 50-row settings list):

| | tokens |
|---|---|
| naive full tree, pretty-printed | ~55,600 |
| after dropping empty fields | ~28,800 |
| compact JSON | ~13,300 |
| **outline — the default** | **~392** |

**142× cheaper**, with nothing lost: all 50 rows' content is still there.

### The idea that does most of the work

Real designs are repetitive. A 50-row list is one shape repeated fifty times,
differing only in text. So every node gets a **structure key** — a hash of its
shape (type, layout, sizing, paint, child structure) that deliberately ignores
ids, names, positions and content. Consecutive siblings sharing a key collapse:

```
10:0 Row item 0 · FRAME 280×48 · row gap8 between/center p8,12 · w:fill h:hug · bg:color/surface/subtle
  10:0a Label · TEXT 120×20 · "Item number 0"
  10:0b Chevron · VECTOR 16×16 · →export
  ↳ ×50 siblings share this shape: 10:0 … 10:49; content: "Item number 0", "Item number 1", … all 50
```

Four lines instead of a hundred and fifty. The id range is stated, so any row is
still addressable, and every content value survives, because that is the data.

**This improves accuracy, not just cost.** The same grouping drives codegen, so a
repeated run becomes a `.map()` — which is what the designer meant and what a
developer would have written:

```tsx
{[
  { label: "Wi-Fi", value: "On" },
  { label: "Bluetooth", value: "Off" },
  { label: "Airplane mode", value: "Off" },
].map((item, i) => (
  <div key={i} className={styles.row}>
    <p className={styles.label}>{item.label}</p>
    <p className={styles.value}>{item.value}</p>
  </div>
))}
```

One template, one CSS rule per shape. Field names come from the Figma layer
names, so the data reads like the design. When the content does not line up —
one row has an extra text node — it falls back to literal copies rather than
inventing a template that does not fit.

### No matter how big the design

The property that matters: a design ten times larger but no more *varied* costs
barely more to read. `tests/token_budget.rs` asserts exactly that — node count can
grow 10× while output grows less than half as fast. A 1,501-node frame collapses
rather than truncating, and still reports its true total.

When something genuinely cannot be collapsed, the outline does **not** silently
truncate. It names the exact resume points:

```
412 node(s) not expanded. Nothing is lost — continue with:
  read_scene node_ids=["3:88", "3:91", "4:12"]
```

Silent truncation is how a cheap read becomes a wrong translation, so nothing is
dropped without saying which ids to ask for next. For a page of many *different*
frames, read it at a shallow depth to get the frame list, then work one frame at
a time — frames are the natural unit, and cost stays flat per frame however many
there are.

### Everything else that costs

- **Nothing empty is serialized.** That tree was emitting ~1,400 `null` fields,
  654 empty arrays and 151 empty objects. Now none.
- **The per-request floor is ~1,500 tokens** for the instructions plus all nine
  tool schemas. That ships with every request, so it is budgeted and tested.
- **`getCSSAsync()` roughly triples the payload**, so it is opt-in and is ignored
  in outline mode rather than silently paid for.
- **`get_tokens` summarises by collection** past 80 variables, since `tokens.css`
  already carries every value.
- **`get_css` caps at 40 nodes** and says how many it did not fetch.
- **`generate_code` needs no `read_scene` first.** One call, not two.
- **JSON compacts above 2 KB**, where indentation stops earning its bytes.

All enforced by `tests/token_budget.rs`, so a change that reintroduces a verbose
default fails CI rather than quietly costing someone their window.

## Motion

Prototype interactions are read, not skipped. Figma's motion model is richer than
CSS's, so each case is handled on its own terms:

| Figma easing | Emitted | Exact? |
|---|---|---|
| `LINEAR`, `EASE_IN`, `EASE_OUT`, `EASE_IN_AND_OUT` | the CSS keyword | yes |
| `CUSTOM_CUBIC_BEZIER` | `cubic-bezier(...)`, same control points | yes |
| `CUSTOM_SPRING` | `linear(...)` stop list from a simulated oscillator | yes |
| `EASE_*_BACK` | a `cubic-bezier` of the same shape | **no** — flagged in the output |

Springs are the interesting one. There is no bezier that reproduces overshoot, so
instead of flattening a spring into an ease-out, the damped oscillator is actually
simulated and sampled into a CSS `linear()` stop list, whose values may exceed 1.
The raw `{mass, stiffness, damping, initialVelocity}` is returned alongside, so
Framer Motion or similar can use the spring natively.

Triggers split by what CSS can express:

- **Hover and press** become `:hover` / `:active`, with the `transition` placed on
  the *base* rule so it animates in both directions. No JavaScript needed.
- **Click, key, drag** become typed, documented React props:
  ```tsx
  export interface ProductCardProps {
    className?: string;
    /** Navigates to "Product detail". Figma transition: smart animate over 600ms. */
    onNavigateToProductDetail?: () => void;
  }
  ```
- **Navigation between frames** is also summarised as a flow graph, which is the
  prototype's routing:
  ```
  Product card --on click--> Product detail
  ```

## Tested under the worst conditions, not just the best

`tests/adversarial.rs` exists because the happy-path tests prove the tool *works*
and prove nothing about whether it *breaks*. Everything the plugin sends is
treated as untrusted: it runs in Figma and speaks over a socket, and a bug in it,
an unexpected API change, or anything else on loopback can put arbitrary bytes on
that wire.

Writing it found five real defects, each of which would have reached a user:

**A deep design aborted the whole process.** Every tree walk — conversion,
hashing, outlining, codegen, and the tree's own `Drop` — is recursive, and none
was bounded. A design nested past the stack limit did not error; it hit
`fatal runtime error: stack overflow` and killed the server, taking the bridge
and all state with it.

**The depth limit was measured on the wrong stack.** The first fix capped depth
at 62, the point where serde_json stops parsing. But a tool call runs on a tokio
worker with a **2 MiB** stack, not the main thread's 8 MiB — and at that size 56
levels survives while 62 aborts. The parse limit was never the real limit; the
stack was. The cap is now 40, with the measurement recorded, and a test that runs
on a deliberately constrained 2 MiB stack so a regression fails CI instead of
crashing someone's server.

**CSS injection through three plugin-supplied strings.** `font-family`,
`line-height`, `letter-spacing` and every key and value of `getCSSAsync()` went
into the stylesheet verbatim. A font named `Inter"; background: url(...)` escaped
its declaration. Values with a known shape are now *validated* against it rather
than stripped — because removing the `;` from `20px; position: fixed` leaves
`20px position: fixed`, which is inert but should never have been written.
Property names must be identifiers, and values carrying `url(`, `@import`,
`expression(` or `javascript:` are dropped rather than repaired.

**Indentation grew O(depth²).** A 1,000-level tree produced **2 MB** of
generated TSX, nearly all of it whitespace.

**A 10,000-character layer name produced a 10,000-character class name.**

What the suite covers now:

| condition | requirement |
|---|---|
| 20,000-level nesting | truncated at the cap, reported in the header, no overflow |
| 10,000 siblings | collapses to under 8k tokens, true total reported |
| empty payload | still produces a valid component |
| negative, 1e300, 1e-300, zero sizes | no panic, no `NaN` or `inf` in CSS |
| `</div><script>`, `{process.env.SECRET}` in text | escaped to entities |
| hostile layer names | sanitised to identifiers; no new CSS rule, braces balanced |
| hostile font / spacing values | no `url(`, no injected declaration, real value preserved |
| CJK, Arabic, emoji, ZWJ, zero-width, RTL override | no char-boundary panic, valid output |
| 1 MB single text node | outline stays under 4k; content kept in the code |
| duplicate node ids | all content survives |
| malformed JSON, bare `{`, unknown kinds, NUL bytes, binary frames | bridge survives and still accepts a valid frame afterwards |
| plugin vanishes mid-request | fails immediately rather than waiting out the timeout |
| 8 concurrent requests answered in reverse order | correlated by id, never cross-wired |

## Known gaps

Stated plainly, because a tool that hides its limits wastes your time:

**Needs the file open.** This reads what Figma has loaded. No plugin panel, no
data. It is not a CI or headless tool — see the roadmap.

**One file at a time.** Cross-file design-system references resolve to names, not
to definitions in the other file.

**Gradient angles are approximate.** Figma stores a 2×3 `gradientTransform`;
we derive an angle from it. Axis-aligned and simply-rotated gradients are right,
skewed ones drift. Exact reproduction needs the full matrix.

**Grid auto-layout is shallow.** `display: grid` and `gap` are emitted, but not
track definitions or per-child placement.

**`FLOAT` variables have no unit in Figma.** Names are the only signal, so tokens
matching `space|gap|size|radius|width|height|padding|margin|inset|stroke` get `px`
and the rest stay unitless. `resolvedType` is returned so you can override.

**Heading levels are guessed** from font size and layer name. `<h2>` for 32px is a
heuristic, not a fact — check it.

**Text is per-node, not per-range.** Mixed styling inside one text layer collapses
to the dominant style; Figma's `figma.mixed` ranges are not split into spans.

**No images, only their hashes.** `IMAGE` fills report `imageHash`; run
`export_assets` on the node to get pixels.

**No blend modes, masks, or vector paths.** Vectors are exported as SVG rather
than reconstructed — correct, but it means they are assets, not markup.

**Hover targets are named, not filled.** A hover that swaps to another variant
emits the transition and a comment naming the destination, but the `:hover` rule
is left empty — the destination variant's styles live in a node we deliberately do
not read. Reading it and diffing the two would close this.

**`AFTER_TIMEOUT` is reported, not generated.** Timed sequences come back with
their delays, but are not turned into `@keyframes`.

**Write is out of scope.** This reads. It never modifies your document.

**Browser caveat:** Chrome and Edge treat `ws://127.0.0.1` as a
potentially-trustworthy origin, so a page on `https://figma.com` connects without
tripping mixed-content blocking. Safari and Firefox are stricter about loopback
WebSockets — use Figma Desktop there, or terminate TLS locally.

## Roadmap

- **Offline `.fig` parsing**, pure Rust, no plugin and no Figma running. The format
  is a ZIP holding a `canvas.fig` Kiwi payload that **embeds its own schema**, so a
  decoder generated from the file survives Figma's continuous deploys. This is the
  path to CI and to files you can export but not open.
- Reading a hover's destination variant and diffing it, to fill the `:hover` rule.
- `@keyframes` for timed sequences.
- Per-range text spans.
- Full grid track reconstruction.
- Vue and Svelte emitters — the IR is framework-agnostic; only `codegen` is not.

## Development

```bash
cargo test                                      # 109 tests
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
node --check plugin/code.js                     # syntax only; not a dependency
```

Four layers of test, all run by CI on Linux, macOS and Windows:

| Suite | What it covers |
|---|---|
| unit (62) | The layout mapping, codegen rules, and motion — including a test that fails if `getCSSAsync()` ever overrides the IR's layout, and one asserting an underdamped spring actually overshoots while an overdamped one does not |
| `tests/bridge_roundtrip.rs` (8) | The real WebSocket server driven by a fake plugin — wire protocol, IR, motion and codegen end to end, without Figma |
| `tests/plugin_consistency.rs` (7) | Rust/plugin drift: port agreement across `main.rs`, `manifest.json` and `ui.html`; every op the server calls exists in the plugin and none are dead; no synchronous Figma APIs under `documentAccess: dynamic-page` |
| `tests/lsp_protocol.rs` (7) | The real LSP client against a fake language server over an in-memory pipe — framing, handshake, request correlation, fuzzy-match rejection, `node_modules` deprioritisation, and that a hung server times out instead of blocking generation |
| `tests/adversarial.rs` (17) | Best case and worst case: pathological depth, 10k siblings, hostile strings and numbers, unicode, malformed frames, mid-request disconnects, concurrent correlation |
| `tests/mcp_stdio.rs` (3) | A real `initialize` / `tools/list` / `tools/call` handshake against the compiled binary, asserting stdout is clean JSON-RPC |
| `tests/token_budget.rs` (7) | Enforced token budgets on a realistic 151-node screen, that cost grows sub-linearly with design size, that the fidelity levels are genuinely priced apart, and the per-request schema floor |

The drift guards exist because those mismatches fail *silently at runtime* — a
stale port is a socket that never opens, and Figma reports a plugin syntax error as
a blank panel with an empty console.

## A note on scope

This uses Figma's documented Plugin API through a normal development plugin. It
does not reverse-engineer Figma's internals, scrape the canvas, or circumvent any
limit — it uses a supported extension point that happens to be more capable than
the REST API.

Not affiliated with or endorsed by Figma.

## License

MIT — see [LICENSE](LICENSE).
