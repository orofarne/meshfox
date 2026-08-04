// Mirrors crates/core/src/canvas.rs's Node/Canvas (JSON shape, camelCase).

export type NodeType = "text" | "file" | "link" | "group" | "constraint";

/** A `constraint` node's most recently evaluated result — mirrors
 * crates/core/src/constraint.rs's `ConstraintStatus`. Present only once the
 * server has evaluated it (always, as of `GET /api/canvas` — see
 * `crates/server/src/lib.rs`'s `canvas_response`); absent means "not
 * evaluated by this response", not "passed". */
export interface ConstraintStatusDto {
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
   * user actually drags/resizes it or `meshfox fmt` gives it a real one.
   * The server sends exactly what's in the file, no computed fallback. */
  x?: number;
  y?: number;
  width?: number;
  height?: number;
  color?: string;
  /** Free-form labels — purely descriptive, no structural meaning. Absent
   * or empty means "no tags". */
  tags?: string[];
  /** For file/link nodes: the path or URL parsed out of the body's one link. */
  target?: string;
  /** file-node only: how the target is shown — a plain link (default,
   * absent here) or a read-only code preview of the target file's content. */
  display?: "link" | "code";
  /** file-node only: syntax-highlighting language hint for display="code";
   * absent means auto-detect from the target's file extension. */
  lang?: string;
  text: string;
  /** `constraint`-node only — see `ConstraintStatusDto`. */
  constraintStatus?: ConstraintStatusDto;
}

export interface CanvasDoc {
  nodes: CanvasNode[];
}

// Mirrors crates/server/src/lib.rs's `VarStatus` (JSON shape, camelCase) —
// one declared `meshfox:var` per entry, see SPEC.md's "Variables". `value`
// is only present when `resolved` is true and the variable isn't `secret`
// — a secret is never sent to the browser even if the server process's own
// environment already resolved it, since there's no reason to.
export interface VarStatus {
  name: string;
  type: "string" | "int" | "bool" | "select";
  prompt: string;
  choices?: string[];
  secret: boolean;
  resolved: boolean;
  value?: string;
}
