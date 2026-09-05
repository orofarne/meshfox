// Mirrors crates/core/src/canvas.rs's Node/Canvas (JSON shape, camelCase).

// "include" only ever appears here as something the client *writes* (via
// NodeSettings' type dropdown + `NodePatch.nodeType`) — the server always
// resolves an include node into a "group" or "text" node before it's ever
// sent back over `GET /api/canvas` (see crates/core/src/include.rs), so
// `CanvasNode.type` itself never actually carries this value on read.
export type NodeType = "text" | "file" | "link" | "group" | "include";

/** One embedded ` ```starlark constraint ` fence's most recently evaluated
 * result — mirrors crates/core/src/constraint.rs's `ConstraintStatus`. A
 * node can carry several (see `CanvasNode.constraintResults`); present at
 * all means the server has evaluated it (always, as of `GET /api/canvas` —
 * see `crates/server/src/lib.rs`'s `canvas_response`). */
export interface ConstraintStatusDto {
  /** Display identifier for this fence specifically: the explicit
   * `name="..."` (as `"<node-id>/<name>"`) if given, else the node's own id
   * when it's the node's only constraint, else `"<node-id>#<n>"`. */
  label: string;
  ok: boolean;
  /** Every `fail(msg)` call the script made, or the one parse/runtime/
   * resource-limit error if it didn't run to completion. Empty when `ok`. */
  messages: string[];
}

/** An extra incoming edge (`meshfox:edge from="..."`), plus optional
 * per-edge styling — mirrors crates/core/src/canvas.rs's `ExtraEdge`.
 * Every field but `from` is `undefined` when the author never set it; a
 * renderer is free to pick its own default in that case (see App.tsx,
 * which keeps the pre-existing dashed/arrow-end look for such an edge). */
export interface ExtraEdgeDto {
  from: string;
  label?: string;
  color?: string;
  style?: "solid" | "dashed" | "dotted";
  arrowStart?: "none" | "arrow";
  arrowEnd?: "none" | "arrow";
  /** Free-form labels — purely descriptive, no structural meaning. Absent
   * or empty means "no tags". */
  tags?: string[];
}

export interface CanvasNode {
  id: string;
  title: string;
  level: number;
  /** Absent means "text" — meshfox omits the default from the wire format. */
  type?: NodeType;
  parent?: string;
  extraParents?: ExtraEdgeDto[];
  /** Absent means unpositioned — the web client lays such a node out
   * itself (see `./autolayout.ts`), never persisting the result unless the
   * user actually drags/resizes it into a real one. The server sends
   * exactly what's in the file, no computed fallback. */
  x?: number;
  y?: number;
  width?: number;
  height?: number;
  color?: string;
  /** This node's own `color`, or, when absent, a fallback derived from its
   * tags against the document's `meshfox:tag-color` defaults — mirrors
   * `crates/core/src/canvas.rs`'s `Node.effective_color`
   * (`crates/core/src/tag_colors.rs`). Always present when `color` is;
   * use this (not `color`) for anything that *renders* a node's color —
   * `color` itself stays the raw, editable value (see NodeSettings). */
  effectiveColor?: string;
  /** Per-node fold-state override (`fold="true"`/`fold="false"` on
   * `meshfox:node`) — see SPEC.md's "Options" section. Absent means "no
   * override": follow the document's own default (`CanvasDoc.options`'
   * `unfold`, see App.tsx's `resolveDefaultFold`). */
  fold?: boolean;
  /** Free-form labels — purely descriptive, no structural meaning. Absent
   * or empty means "no tags". */
  tags?: string[];
  /** For file/link nodes: the path or URL parsed out of the body's one link. */
  target?: string;
  /** file/link/include only: optional plain-prose explanatory text after
   * the required link (SPEC.md: body is exactly one Markdown link,
   * optionally followed by a caption) — absent means no caption. Inline
   * formatting only (bold/italic/code/links); rendered the same way a
   * `text` node's body is. */
  caption?: string;
  /** file-node only: how the target is shown — a plain link (default,
   * absent here) or a read-only code preview of the target file's content. */
  display?: "link" | "code";
  /** file-node only: syntax-highlighting language hint for display="code";
   * absent means auto-detect from the target's file extension. */
  lang?: string;
  /** file-node only: an executable (e.g. "python") to run against
   * `target`, making the node runnable — absent means it isn't. */
  interpreter?: string;
  /** link-node only: whether to fetch and show an OpenGraph social preview
   * (title/description/image) below the link. Absent means off (the
   * default) — meshfox omits the default from the wire format, same as
   * `type`. */
  preview?: boolean;
  /** Label text for the *structural* edge from `parent` into this node
   * (`edgeLabel=` on `meshfox:node`) — see `crates/core/src/canvas.rs`'s
   * `Node.edge_label`. Absent for the root and for any node that's never
   * had one set. Purely descriptive text; unlike `ExtraEdgeDto`, a
   * structural edge has no color/style/arrowhead attributes to go with
   * it. */
  edgeLabel?: string;
  text: string;
  /** Results of every embedded ` ```starlark constraint ` fence in this
   * node's own body, in document order — see `ConstraintStatusDto`. Absent
   * or empty means this node has no constraint fences. */
  constraintResults?: ConstraintStatusDto[];
  /** Absolute directory a relative asset reference (an `![](...)` image, or
   * a plain link) in `text` should resolve against, when it's not the
   * canvas file's own directory — set when this node's body was spliced in
   * from an `include` target that lives elsewhere on disk (see
   * `crates/core/src/include.rs`). Absent for every node that wasn't. */
  assetBase?: string;
  /** `true` when this node's `text` is actually a plain-Markdown `include`
   * target's transcluded content (shifted headings and all — see
   * `crates/core/src/include.rs`'s `resolve`), not this node's own real
   * text — unlike a canvas-`include` descendant (which has a real,
   * separate on-disk identity and is safely editable per-node), there's
   * no well-defined way to write a per-node body edit here back to "the
   * target file". `NodeTextEditor`'s caller uses this to redirect into
   * Source mode (already scoped to this node's own id, which doubles as
   * the include's `nodeId` — see `fetchIncludes`) instead of opening the
   * normal inline editor, which could only ever fail to save. Absent
   * (falsy) for every other node. */
  plainMarkdownInclude?: boolean;
}

export interface CanvasDoc {
  nodes: CanvasNode[];
  /** Every `<!-- meshfox:option name="..." -->` this document declares
   * (see `crates/core/src/options.rs`, SPEC.md's "Options" section) — e.g.
   * `"unfold"`, which flips the web UI's own default fold state for the
   * whole document. Absent/empty means none declared. */
  options?: string[];
}

// Mirrors crates/server/src/lib.rs's `VarStatus` (JSON shape, camelCase) —
// one declared `meshfox:var` per entry, see SPEC.md's "Variables". `value`
// is omitted for a `secret` variable regardless of `resolved` — a secret is
// never sent to the browser even if the server process's own environment
// already resolved it, since there's no reason to. For a non-secret
// variable, `value` can be present even when `resolved` is false: a
// `required` declaration that's still unconfirmed still carries its own
// `default` here, purely as VarsForm's pre-filled suggestion.
export interface VarStatus {
  name: string;
  type: "string" | "int" | "bool" | "select";
  prompt: string;
  choices?: string[];
  secret: boolean;
  resolved: boolean;
  value?: string;
}
