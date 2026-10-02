# Markdown in meshfox

A node's own body (and this file's own prose) is Markdown — but "Markdown"
here means CommonMark plus a specific, deliberately narrow set of
extensions, some borrowed from GitHub/GFM, some meshfox's own. This is the
reference for that surface: what's supported, in what syntax, and — since
meshfox renders the same document four different ways (web canvas, `meshfox
tui`, static-site export, PDF export) through two independent Markdown
engines (`react-markdown`/`remark` on the web side, `pulldown-cmark` on
every Rust side) — which of those four actually render each one.

This is about *Markdown itself*. For `meshfox:*` HTML-comment bookkeeping
(`meshfox:node`, `meshfox:var`, ...) see [SPEC.md](./SPEC.md) instead —
including its own "Formal grammar" section for that syntax's EBNF.

## Headings become nodes

The single biggest departure from plain Markdown: a heading isn't just a
heading here, it's the tree structure.

- The document's first `#` is always the **root** node, whether or not
  it's marked.
- Any other heading (`##`...`######`) becomes a node *only if* immediately
  followed by a `<!-- meshfox:node ... -->` comment — an unmarked heading
  is just prose, free to use for sub-structure inside a node's own body
  without fragmenting the canvas.
- A node's parent is normally the nearest enclosing shallower node
  heading. `######` (H6) is CommonMark's own depth ceiling — nesting
  further than that means writing more `######` headings and pointing
  `parent=` at the real parent explicitly, since heading depth alone can't
  express it past six levels.

Full attribute reference (`id`, `type`, `x`/`y`, `tags`, `parent`, ...):
SPEC.md's "File structure" and "Node types" sections.

## Already-standard extensions

Enabled beyond bare CommonMark, before any of meshfox's own additions:

| Feature | Syntax | web | static/PDF | TUI |
|---|---|---|---|---|
| Tables | `\| a \| b \|` | yes | yes | yes |
| Strikethrough | `~~text~~` | yes | yes | yes |
| Task lists | `- [ ] x` | yes | yes | yes — a `[ ]`/`[x]` appended after the item's own bullet/number marker |
| Footnotes | `[^1]` | yes | yes | yes — reference renders as real Unicode superscript when the label maps fully (the common numeric case), else a bracketed literal (`[note]`); the definition gets its own segment, a bracketed label line followed by its body |

`pulldown-cmark`'s task-list/footnote support introduces its own event
kinds (`Event::TaskListMarker`, `Event::FootnoteReference`,
`Tag::FootnoteDefinition`) that TUI's hand-rolled renderer
(`crates/cli/src/tui/markdown.rs` — hand-rolled over the event stream
rather than a ready-made "Markdown to terminal" crate, see its own module
doc) needed explicit handling for; the two `Options` flags stayed off
until that handling existed, since turning them on without it would have
silently dropped the checkbox/reference marker rather than showing plain
literal text. One real divergence from web/static: a footnote reference's
display number isn't renumbered into first-reference order the way
`pulldown-cmark`'s own HTML writer does (that needs a lookahead pass over
the whole document; TUI's renderer is a single streaming pass) — it shows
the label as written, which in practice is already the sequence number a
document's author wants shown.

## meshfox's own narrow extensions

Three come from `TODO.canvas.md`'s "markdown-extensions" discussion — kept
deliberately narrow (a small, well-defined grammar) rather than adopting
any single flavor's full feature set wholesale, since every one of them
needs an independent implementation on both the Rust side and the web
side (`pulldown-cmark` and `remark` share nothing with each other).

### Image size and background (`{width=..}`/`{height=..}`/`{bg=..}`)

GitLab/Pandoc-style, written with no space directly after an image's
closing `)`:

```markdown
![alt](pic.png){width=300}
![alt](pic.png){height=50%}
![alt](pic.png){width=300 height=50%}
![alt](diagram.svg){bg=#fff}
![alt](diagram.svg){width=50% bg=transparent}
```

Only `width=`/`height=` (each a bare integer — pixels — or integer+`%`) and
`bg=` (`#rgb`, `#rrggbb` or `transparent`, any case), each at most once —
not Pandoc's full `{.class #id ...}` attribute grammar, and never free-form
CSS. Anything that doesn't match this exact shape is left alone as ordinary
literal text.

`bg=` paints a fixed backing color behind the image — for a transparent SVG
drawn for a white page (Mermaid, PlantUML, a formula), which is unreadable
on a dark theme. There is deliberately no global rule: you choose per image.
`bg=transparent` says "no backing" explicitly (the default).

| | web | static/PDF | TUI |
|---|---|---|---|
| `width`/`height` | full — real `width`/`height` on the rendered `<img>` | full, same as web | `%` only, scales the terminal image protocol's fixed size budget; a literal pixel value is parsed but has no effect (no pixel grid to map it onto) |
| `bg` | `style="background:#rrggbb"` on the `<img>` | same | painted behind the SVG when it is rasterized; only SVGs are rasterized by meshfox, so it has no effect on a PNG/JPEG (those carry their own pixels) |

Shared parser: `crates/core/src/image_attrs.rs` (Rust) /
`web/src/remarkImageAttrs.ts` (web).

### SVG images

An SVG is an ordinary Markdown image — a relative `.svg` file, or a
`data:image/svg+xml;base64,…` URL (what `output="image"` / `output="markdown"`
blocks produce; see SPEC.md's "Image output" and `examples/svg.canvas.md`) —
and shows on all four platforms. Inline `<svg>` HTML is **not** supported
(raw HTML is dropped, same as for any other element): that keeps scripts out
of the page, and an SVG shown through `<img>` can't run any.

| | web | static/PDF | TUI |
|---|---|---|---|
| Support | `<img>`; the page's theme is injected (below) | `<img>`; no theme injection (a static page has no live theme to follow — use `bg=` for an SVG drawn for a white page) | rasterized with `resvg` (always the dark theme, like the rest of the TUI); an SVG's embedded raster `<image>` is skipped |

**Theme.** An SVG shown through `<img>` can't see the page's CSS, so a
diagram drawn black-on-transparent vanishes on a dark theme. Right before
showing a `data:image/svg+xml;base64,…` image, the web UI and the TUI
rewrite the SVG's *text* (the file keeps what the tool produced), giving a
tool two ways to follow the theme:

- `currentColor` becomes the theme's text color;
- `var(--mf-fg)`, `var(--mf-bg)`, `var(--mf-accent)`, `var(--mf-border)`
  become the theme's colors. Write a fallback —
  `var(--mf-fg, #000)` — so a viewer that knows nothing about meshfox
  (GitHub, a plain browser) still draws sensible colors.

A tool's own explicit colors (`fill="#ffd166"`) are never touched, and
neither are SVGs behind a relative file path (only inline `data:` URLs are
rewritten). Shared logic: `crates/core/src/svg.rs` (Rust; the TUI also
substitutes `var()` textually, since `resvg` has no CSS-variable support at
all) / `web/src/svgTheme.ts` (web, where the browser resolves `var()` from
the injected `:root` properties itself).

### Subscript / superscript (`x~2~` / `x^2^`)

Pandoc/kramdown-style: a single (not doubled) `~`/`^`, content with no
internal whitespace, non-empty.

```markdown
H~2~O and E=mc^2^
```

Deliberately narrow for the same reason as image size — and, on the web
side specifically, narrow enough to need disambiguating from GFM's own
strikethrough (`~~text~~`, and — looser than `pulldown-cmark` — GFM
strikethrough also accepts a single, non-doubled `~`): `remarkSubSup.ts`
reclaims exactly the shape this grammar defines (single tilde, no internal
or flanking whitespace) back from `remark-gfm`'s strikethrough parsing;
anything wider (`~~doubled~~`, or a single-tilde run with a space inside
or around it) stays real strikethrough on both sides.

| | web | static/PDF | TUI |
|---|---|---|---|
| Support | real `<sub>`/`<sup>` elements | real `<sub>`/`<sup>` elements | Unicode small-form character substitution (`₂`, `ⁿ`, ...) where a full mapping exists for every character in the marked run; falls back to the literal `~text~`/`^text^` source otherwise — coverage is genuinely incomplete (e.g. no subscript for `q`/`b`/`c`/`d`/`f`/`g`/`w`/`y`/`z`, no uppercase at all) |

Shared scanner: `crates/core/src/subsup.rs` (Rust) /
`web/src/remarkSubSup.ts` (web, plus its own reclaim pass).

### GFM alert blockquotes (`> [!NOTE]`/...)

GitHub's alert syntax — a blockquote whose first line is exactly one of
`[!NOTE]`, `[!TIP]`, `[!IMPORTANT]`, `[!WARNING]`, `[!CAUTION]`:

```markdown
> [!WARNING]
> Be careful here.
```

The marker line is never shown — it's parsed out entirely, not just
hidden by CSS.

| | web | static/PDF | TUI |
|---|---|---|---|
| Support | full — icon + label + color via CSS (`.markdown-alert-*`) | full, same CSS scheme (`site-template/style.css`) | full — a colored icon+label title line ahead of the (otherwise ordinary) quoted body |

On the Rust side this is native: `pulldown-cmark`'s own `Options::
ENABLE_GFM` parses and strips the marker, handing back
`Tag::BlockQuote(Some(BlockQuoteKind))` — no hand-rolled parsing needed.
`remark-gfm` doesn't cover alerts (they're a GitHub UI convention, not
part of the GFM spec it implements), so the web side has its own small
plugin instead: `web/src/remarkGfmAlerts.ts`.
