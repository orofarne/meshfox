# Writing a `meshfox static` template

`meshfox static` turns a canvas into a static site through a template: a
directory of [Tera](https://keats.github.io/tera/) pages plus whatever assets
those pages need. This file is the contract between meshfox and a template —
what a template receives, what meshfox does to it, and what it must do in
return. Two templates ship in this repo and follow it:

- [`site-template/`](./site-template) — the project-page layout.
- [`site-template-archive/`](./site-template-archive) — the investigation
  board used for this repo's own published site.

**Template API version: 1.** `template.toml` must say so (see
[`template.toml`](#templatetoml)); a template written for another version is
refused instead of being rendered against context it does not expect. The
version changes whenever a change could break a template that ignores it:
a renamed or removed context key or field, a different type, a different
escaping or file-handling rule.

```sh
meshfox static README.md --template site-template -o /tmp/site --force
```

See `meshfox static -h` for the command's own flags (`--copy-files`,
`--recursive`, `--sitemap`, ...); this file is about the template side.

## Directory layout

| In the template directory | What `meshfox static` does with it |
|---|---|
| `*.tera` | Rendered, and written to `--out` at the same relative path **minus `.tera`** (`index.html.tera` → `index.html`). |
| `*.tera` whose basename starts with `_` | A **partial**: loaded so other pages can `{% import %}` it, never rendered as a page. |
| `template.toml` | The template's own settings. Read, never copied to `--out`. |
| anything else | Copied to `--out` verbatim (CSS, JS, fonts, images). |

There is no required page, but a site needs an `index.html.tera`. All pages
are rendered with the same context.

## `template.toml`

Optional as a file (a template without one gets no `base_url`, no
`links_base_url`, no `icons`), but if it exists it **must** contain
`api_version`, and **any key not listed here is an error** — a typo is
reported instead of being silently ignored.

| Key | Type | Meaning |
|---|---|---|
| `api_version` | integer, required | The template API version this template was written against: `1`. |
| `base_url` | string | This site's own canonical absolute URL, with no trailing path (`https://example.com`). Prefix of every `<loc>` in the `--sitemap` file, which is refused without it. Not used to rewrite links. |
| `links_base_url` | string | Prefixed onto a relative link that `static` does not copy into `--out` (a plain Markdown link, or a `file`/`link` node's target when it is not `display="code"`). Use it when the canvas's own files are published somewhere other than the site, e.g. a repository. Images and `display="code"` targets are unaffected. |
| `[[icons]]` | array of tables | `<link>` tags for the page's icons, exposed to templates as `icons`. Each has `rel` and `href` (required), and optional `sizes` and `type`. Each `href` should be a relative path to a file the template itself ships; nothing is fetched or copied from elsewhere. Unknown keys inside an `[[icons]]` table are an error too. |

`base_url` and `links_base_url` differ on purpose: the first is where *this
site* lives, the second is where the *content it points at* lives.

## Context

Every page is rendered with these variables:

| Variable | Type | Meaning |
|---|---|---|
| `site` | `SiteData` | The canvas: `site.title`, `site.root` (a `NodeView` tree), `site.edges`. Described below. |
| `icons` | list of `{rel, href, sizes?, type?}` | The `[[icons]]` of `template.toml`; empty when there are none. A key that is not set is absent — test it with `{% if icon.sizes %}`. |
| `meshfox_version` | string | The version of the `meshfox` binary doing the export. |
| `canvas_commit` | string, maybe absent | The short git `HEAD` of the repository the canvas lives in. Absent outside a git working tree, so use `{% if canvas_commit %}` or `default(value=...)`. |

With `--recursive`, each nested canvas is rendered with the same template into
its own subdirectory, and `site` is *that* canvas's data. The context does not
say where in the site a page lives; use relative links.

### `SiteData`

| Field | Type | |
|---|---|---|
| `title` | string | The canvas's title (its root node's title). |
| `root` | `NodeView` | The root of the tree; there is always exactly one. |
| `edges` | list of `EdgeView` | Every structural and extra connection. |

### `NodeView`

Fields marked *optional* are **absent** (not `null`) when they have no value;
test them with `is defined` or `{% if %}`.

| Field | Type | |
|---|---|---|
| `id` | string | The node id. |
| `title` | string | The node title, plain text. |
| `level` | integer | The Markdown heading level the node was written at. It can differ from `depth`. |
| `depth` | integer | Real depth in the tree; the root is `0`. |
| `node_type` | string | `"text"`, `"file"`, `"link"` or `"group"`. (`"include"` only appears for a canvas that was never resolved, which `meshfox static` always does first.) |
| `position` | `Position`, optional | Present only when the node has all of `x`, `y`, `width` and `height` authored. Render at exactly these pixels; a node without it is plain flowed HTML and the browser sizes it. `x`/`y` are absolute: for a member of a `group` they are its group-relative coordinates resolved against the group's. Absent when a group above the node has no position of its own to resolve against. |
| `authored_position` | `Position`, optional | The same four values exactly as written on the node, before a group's relative positions are resolved. A group can lay its children out on its own local board with these. |
| `spatial_children` | boolean | Every direct child has an authored box, so the template may draw them on a coordinate board inside this node. |
| `color` | string, optional | The node's own `color`, or one derived from its tags and the canvas's `meshfox:tag-color` defaults, as a CSS color. A numbered preset (`1`–`6`) has already been turned into its hex value. |
| `tags` | list of strings | |
| `html_body` | string | The node's Markdown body as HTML. Empty for a `group`. **Already markup: output it with `\| safe`** (see [Escaping and trust](#escaping-and-trust)). |
| `target` | string, optional | The target of a `file` or `link` node. |
| `foldable` | boolean | Whether folding would hide anything: `false` for a title-only node without children. |
| `folded` | boolean | Whether the node starts collapsed. A starting point only; never `true` when `foldable` is `false`. |
| `children` | list of `NodeView` | Direct children in document order. Walk the tree with a recursive macro (see the shipped `_macros.html.tera`). |

`Position` is `{x, y, width, height}`, all numbers.

### `EdgeView`

`site.edges` lists every structural (parent → child) edge and every
`meshfox:edge`. The scripts in the shipped templates read exactly this list as
JSON.

| Field | Type | |
|---|---|---|
| `from`, `to` | string | Node ids. |
| `kind` | string | `"tree"` for a parent → child edge, `"extra"` for a `meshfox:edge`. |
| `label` | string, optional | |
| `color` | string, optional | A CSS color, set only on an `"extra"` edge (a numbered preset is already hex). |
| `label_at` | integer or `null` | Label position along the edge, `0`–`1000`; `null` means the middle. |
| `source_side`, `target_side` | `"left"`, `"right"`, `"top"`, `"bottom"` or `null` | The side the author pinned; `null` means choose automatically. |
| `via` | list of `{x, y}` | Authored waypoints, relative to the source port. May be empty. |
| `style` | string | `"solid"`, `"dashed"` or `"dotted"`. A `"tree"` edge is always `"solid"`. |
| `arrow_start`, `arrow_end` | boolean | |

`label` and `color` are absent when unset, while `label_at`, `source_side` and
`target_side` are `null`; keep that in mind when testing them in a template
(the JSON handed to a script has the same shape).

## Filters

Everything Tera ships, plus one of meshfox's own:

| Filter | |
|---|---|
| `script_json` | Any value as JSON that is safe inside a `<script type="application/json">` element. See [Escaping and trust](#escaping-and-trust). |

## Escaping and trust

Every `*.tera` page is **auto-escaped**: `{{ n.title }}`, `{{ t }}` for a tag,
`{{ icon.href }}` and every other value are HTML-escaped, whatever the page's
name. (Tera by itself only escapes names ending in `.html`, `.htm` or `.xml`;
meshfox turns it on for `.tera` pages because ours end in `.tera`.) Do not
disable it. A node title or tag is untrusted text and can contain markup.

Two values are markup and must opt out with `| safe`:

- `n.html_body`;
- `site.edges | script_json`, inside a `<script type="application/json">`.

**Use `script_json`, not Tera's `json_encode`, to put data in a script
element.** `json_encode` leaves `</script>` as it is, so an edge label
containing it would end the element and the rest of the label would run as
markup. `script_json` is `json_encode` with `<`, `>`, `&` and the two Unicode
line separators written as `\uXXXX`; a JSON parser reads exactly the same data
back.

What meshfox has already done to `html_body`, so that `| safe` is acceptable —
the same rules the interactive web UI applies, and the exported page must not
be more permissive than it:

- **Raw HTML is dropped.** `<script>`, `<div onclick=…>` and every other raw
  HTML tag, block or inline, written in a node's Markdown is removed, not
  shown and not sanitized. (HTML that meshfox itself generates is kept: image
  sizes, `<sub>`/`<sup>`, and the code and table markup Markdown produces.)
- **Link and image URLs are restricted.** A URL with a scheme must be
  `http`, `https`, `irc`, `ircs`, `mailto` or `xmpp`; a relative URL, a
  root-relative path, `#anchor` or `?query` is fine. Anything else —
  `javascript:`, `vbscript:`, `data:text/html`, … — becomes an empty URL. An
  image may additionally be a `data:image/…` URI.
- **External links** open in a new tab with `rel="noopener noreferrer"`.

Anything a template writes *outside* these values is the template's own
responsibility; the markup it hard-codes, and its own scripts, are trusted.

## Files referenced from a canvas

- A local image in a node's Markdown is copied into `--out` and its `src`
  rewritten to the copy. It must live inside the canvas's directory (or, for
  a node spliced in by an `include`, inside that include's directory).
- A `file` node with `display="code"` has its target read once at export time
  and its content is part of `html_body` — there is nothing to fetch later.
- Any other relative reference is left alone, or prefixed with
  `links_base_url` when the template sets one. `--copy-files` additionally
  copies a `file` node's target next to the site; with `--recursive` a
  `.canvas.md` target is rendered as a page of its own.

## Connecting arrows: the DOM contract

A static page cannot know where the browser will lay a node out, so arrows are
drawn by a small script once the page has loaded. Both shipped templates use
the same convention; a template that wants arrows should too:

- Render `site.edges` as JSON into `<script id="mesh-edge-data" type="application/json">{{ site.edges | script_json | safe }}</script>`.
- Give every node element the DOM id `node-<id>` (`id="node-{{ n.id }}"`). The
  script looks `from` and `to` up by it. A node that is not in the page — a
  collapsed subtree, say — has no arrow.
- Draw into your own overlay (the shipped `edges.js` files add SVG layers that
  they create themselves and remove before redrawing).

The scripts, the SVG classes and the CSS class names (`.node`, `.node-row`,
`.tree`, `.group-canvas`, …) belong to the template: meshfox does not read or
require any of them.

## Checklist for a new template

1. `template.toml` with `api_version = 1`.
2. Escaping left on; `| safe` only on `html_body` and on `script_json` output.
3. Guard every optional field (`is defined`, `{% if %}`) — see the tables.
4. Relative URLs only for your own assets, so the site works under any path and
   inside each `--recursive` subdirectory.
5. Try it with a canvas whose titles, tags and edge labels contain `<` and
   `&` (and `</script>`), a node with no children, a `group` with authored
   positions, and an edge with a label.
