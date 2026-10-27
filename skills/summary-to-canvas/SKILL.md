---
name: summary-to-canvas
description: Summarize the current conversation (or a pasted text/document) as a meshfox canvas — a `.canvas.md` file: a tree of small Markdown nodes with optional extra edges. Use when the user asks for a meshfox canvas, a "canvas" summary, or to turn a discussion into a mind-map/knowledge-map to open in meshfox.
---

# Summarize a conversation as a meshfox canvas

A meshfox canvas is a plain Markdown file (`name.canvas.md`) where headings are
nodes of a tree. Your job: **structure** the conversation, not retell it.
Output one complete file; the user saves it and opens it with
`meshfox view name.canvas.md` (check it first with `meshfox validate name.canvas.md`).

Write in the language of the conversation. Format keywords (`meshfox:node`,
`id`, `color`, ...) stay exactly as shown.

## Output contract

- Deliver the whole file as a single artifact / file named `<short-slug>.canvas.md`.
  If you cannot create files, give it as one fenced ```` ```markdown ```` block.
- Nothing but the canvas inside it: no preface, no commentary lines.
- After it, one short line: how to open it (`meshfox view <file>`).

## Format (everything you need)

```markdown
<!-- meshfox:canvas -->
# Root title

<!-- meshfox:comment -->
> This is a [meshfox](https://meshfox.orofarne.net/) document — open it
> with `meshfox view` (or `meshfox tui`) for the interactive canvas.
> This note is only visible here, in a plain Markdown viewer.
<!-- /meshfox:comment -->

Root body: topic + 2-3 sentence bottom line.

## Child node
<!-- meshfox:node id="child" color="4" tags="decision" -->

Body of the node: ordinary Markdown.

### Grandchild
<!-- meshfox:node id="grandchild" -->
<!-- meshfox:edge from="other-id" label="because" -->
```

Rules:

1. First line is `<!-- meshfox:canvas -->`. Exactly one `#` heading: the root.
   The root needs no marker. **Always** put the `meshfox:comment` preamble
   (copy it verbatim, in English, as shown) right under the root heading, before
   the root body: it's what `meshfox create` writes, hidden in meshfox and
   visible only in a plain Markdown viewer.
2. A `##`..`######` heading is a node **only if the very next line** is
   `<!-- meshfox:node ... -->`. An unmarked heading is just prose inside the
   enclosing node, so use plain headings for sub-structure within one node.
3. Parent = nearest shallower node heading. Don't skip levels (`##` then `####`).
   Don't go deeper than `######`; split into more top-level branches instead.
4. Node attributes (all optional, `key="value"`, on that one comment line):
   - `id` — stable slug, lowercase `a-z0-9-`, unique. **Always set it** on
     nodes you reference from an edge; setting it on every node is fine.
   - `color` — `"1"` red, `"2"` orange, `"3"` yellow, `"4"` green, `"5"` blue,
     `"6"` purple. Use sparingly and consistently (see conventions).
   - `tags` — comma-separated labels, e.g. `tags="risk,backend"`.
5. Extra edges (beyond the tree): `<!-- meshfox:edge from="id" label="text" -->`
   on the line(s) right after the target node's `meshfox:node` line. It means
   "an arrow from `id` into this node". `from` must be an existing node id;
   `label` is optional. A structural (parent→child) arrow can carry text via
   `edgeLabel="text"` on the child's `meshfox:node`.
6. Node types, by `type="..."` on `meshfox:node`:
   - default `text` — Markdown body;
   - `group` — pure container, **body must be empty** (only its child
     headings); use to bundle related nodes;
   - `link` — body is exactly one line `[label](https://url)`, optionally
     followed (after a blank line) by a caption of plain inline text (no lists,
     headings, code, tables, images).
   Do not use `file` / `include` / any other type.
7. Node bodies: CommonMark + tables, `~~strike~~`, task lists `- [ ]` / `- [x]`,
   footnotes. Don't put runnable code fences with attributes (`name=`,
   `interpreter=`, `cache`, `button`, `form`, ...) in a body — a plain
   ```` ```lang ```` fence for showing code is fine.
8. Never write `x`, `y`, `w`, `h`, `fold`, `createdAt`, `updatedAt`, `parent`
   (except below), edge styling or output/`meshfox:var` markers. Layout is
   automatic.

## Spatial diagrams (rare)

Only when the conversation is really about spatial arrangement (a layout, a
board, an architecture sketch): put the diagram inside a **`group`** node and
give only its direct children `x`/`y`/`w`/`h` — relative to the group's own
corner, so it cannot disturb the rest of the canvas. Everything outside such a
group stays coordinate-free.

```markdown
## Architecture sketch
<!-- meshfox:node id="arch" type="group" -->

### API
<!-- meshfox:node id="arch-api" x="0" y="0" w="200" h="80" -->

### DB
<!-- meshfox:node id="arch-db" x="300" y="0" w="200" h="80" -->
<!-- meshfox:edge from="arch-api" label="SQL" -->
```

## How to summarize

Root: the topic as the title; body = what was discussed and the bottom line in
2-3 sentences. Then branches, only those that the conversation actually has:

- **Topics / facts** — one node per distinct theme; the title is the claim
  ("Redis is overkill at this load"), not a label ("Redis").
- **Decisions** (`color="4"`) — what was decided and why, in a few lines.
- **Open questions** (`color="3"`) — unresolved items.
- **Risks / blockers** (`color="1"`).
- **Action items** (`color="5"`) — a task list `- [ ]`, with owner and deadline
  when named; `- [x]` for what is already done.
- **Sources** — a `group` of `link` nodes for URLs mentioned.

Guidelines:

- Prefer many small nodes (a few lines to a short list/table) over a few walls
  of text. 5-20 nodes is typical; depth 2-3.
- Use edges for cross-links the tree cannot express: decision → risk,
  question → blocks an action, option → chosen. Don't edge everything.
- Keep concrete details that matter: names, numbers, dates, commands, links,
  quotes. Drop greetings, false starts and repeats.
- Record only what was said. Mark your own inference as such; don't invent
  owners, deadlines or decisions.
- Don't dump a transcript: no "User asked / Assistant answered" structure.

## Before you answer — checklist

- First line is `<!-- meshfox:canvas -->`; exactly one `#`, followed by the
  verbatim `meshfox:comment` preamble.
- Every marked node has the `meshfox:node` comment as the *next* line after its
  heading; every `id` is unique.
- Every `edge from="..."` points at an existing `id`.
- `group` bodies are empty; `link` bodies are a single `[label](url)` line.
- No `x`/`y`/`w`/`h` outside a spatial `group`.

A full small example is in `example.canvas.md` next to this file.
