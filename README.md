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
| `read_scene` | The reconstruction: geometry, layout semantics, paint, text, tokens, component identity |
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
cargo test                                      # 44 tests
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
node --check plugin/code.js                     # syntax only; not a dependency
```

Four layers of test, all run by CI on Linux, macOS and Windows:

| Suite | What it covers |
|---|---|
| unit (27) | The layout mapping, codegen rules, and motion — including a test that fails if `getCSSAsync()` ever overrides the IR's layout, and one asserting an underdamped spring actually overshoots while an overdamped one does not |
| `tests/bridge_roundtrip.rs` (7) | The real WebSocket server driven by a fake plugin — wire protocol, IR, motion and codegen end to end, without Figma |
| `tests/plugin_consistency.rs` (7) | Rust/plugin drift: port agreement across `main.rs`, `manifest.json` and `ui.html`; every op the server calls exists in the plugin and none are dead; no synchronous Figma APIs under `documentAccess: dynamic-page` |
| `tests/mcp_stdio.rs` (3) | A real `initialize` / `tools/list` / `tools/call` handshake against the compiled binary, asserting stdout is clean JSON-RPC |

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
