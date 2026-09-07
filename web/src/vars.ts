// Client-side mirror of crates/core/src/vars.rs — just enough of
// `meshfox:var` declaration parsing and `close_over_var_refs` to compute a
// runnable block's *implicit* dependencies (a variable it references,
// directly or through default_var=/choices_var=, that's itself
// from=-computed by another block) for MeshNode's own display. Same
// relationship `./deps.ts` already has to `crates/core/src/deps.rs` — this
// never needs to be byte-for-byte identical, just close enough for a UI
// hint. In particular, `validate_var_scope`'s node-subtree scoping is a
// `meshfox validate`-only lint (`declared_vars` itself doesn't enforce it),
// so this mirrors `declared_vars`'s flat, whole-document namespace and
// skips scope-checking entirely.

import type { CanvasDoc } from "./types";
import { attrsFromTokens, tokenize } from "./fence";
import { type BlockAddr, parseBlockRef } from "./deps";

export interface ClientVarDecl {
  name: string;
  from?: BlockAddr;
  defaultVar?: string;
  choicesVar?: string;
}

/** Parses one `<!-- meshfox:var ... -->` line's attributes into a
 * declaration — direct port of `vars.rs`'s `parse_var_comment` +
 * `build_var_decl`'s own name/from/default_var/choices_var handling (every
 * other attribute — type=, choices=, secret=, ... — is irrelevant to
 * computing implicit deps, so it's not parsed here at all). `ownerNodeId`
 * resolves a bare `from="block-name"` (no `node-id/`) the same way
 * `scan_all_var_decls` does: against the node the declaration itself lives
 * in. Returns `null` for a line with no `name=`, or one that isn't a
 * `meshfox:var` comment at all (mirrors `MissingName` being a hard error
 * server-side; here it just means "not usable for this UI hint"). */
function parseVarDeclLine(line: string, ownerNodeId: string): ClientVarDecl | null {
  const trimmed = line.trim();
  if (!trimmed.startsWith("<!--") || !trimmed.endsWith("-->")) return null;
  const inner = trimmed.slice(4, -3).trim();
  if (!inner.startsWith("meshfox:var")) return null;
  const rest = inner.slice("meshfox:var".length).trim();
  const attrs = attrsFromTokens(tokenize(rest));
  const name = attrs.name;
  if (!name) return null;
  const from = attrs.from ? parseBlockRef(attrs.from, ownerNodeId) : undefined;
  return {
    name,
    from,
    defaultVar: attrs.default_var,
    choicesVar: attrs.choices_var,
  };
}

/** Every `meshfox:var` this document declares, across every node (not just
 * root — see `declared_vars`'s own doc comment), keyed by name. Like
 * `declared_vars`, a document-wide flat namespace; unlike it, a duplicate
 * name here just means "last one wins" rather than a hard error — this is
 * a best-effort UI mirror, not authoritative validation. */
export function parseVarDecls(canvas: CanvasDoc): Map<string, ClientVarDecl> {
  const decls = new Map<string, ClientVarDecl>();
  for (const node of canvas.nodes) {
    for (const line of node.text.split("\n")) {
      const decl = parseVarDeclLine(line, node.id);
      if (decl) decls.set(decl.name, decl);
    }
  }
  return decls;
}

/** Every variable name transitively reachable from `seed` by following
 * `defaultVar`/`choicesVar` references — direct port of `vars.rs`'s
 * `close_over_var_refs`. */
export function closeOverVarRefs(decls: Map<string, ClientVarDecl>, seed: Iterable<string>): Set<string> {
  const closure = new Set<string>();
  const frontier = Array.from(seed);
  while (frontier.length > 0) {
    const name = frontier.pop()!;
    if (closure.has(name)) continue;
    closure.add(name);
    const decl = decls.get(name);
    if (decl) {
      for (const ref of [decl.defaultVar, decl.choicesVar]) {
        if (ref && !closure.has(ref)) frontier.push(ref);
      }
    }
  }
  return closure;
}

/** A whole-token `$NAME` reference in a shebang-style `interpreter=`
 * string — a naive whitespace split rather than `core::exec::
 * interpreter_var_refs`'s real shell tokenization (`shlex`), since this is
 * only ever a best-effort UI hint, never used to actually resolve/spawn
 * anything. Doesn't handle a quoted argument containing whitespace the way
 * the Rust side does — acceptable here, same spirit as this file's other
 * simplifications. */
export function interpreterVarRefsNaive(spec: string | undefined): string[] {
  if (!spec) return [];
  const names: string[] = [];
  const seen = new Set<string>();
  for (const word of spec.split(/\s+/)) {
    const m = /^\$([A-Za-z_]\w*)$/.exec(word);
    if (m && !seen.has(m[1])) {
      seen.add(m[1]);
      names.push(m[1]);
    }
  }
  return names;
}

export interface ImplicitDep {
  varName: string;
  source: BlockAddr;
}

/** This block's own implicit dependencies: every declared variable its
 * `env=`/`interpreter=` transitively needs (through `defaultVar`/
 * `choicesVar` chains) that's itself `from=`-computed — direct port of
 * `deps.rs`'s `implicit_from_deps`, just returning the variable name
 * alongside its source address (unlike the Rust original) since there's no
 * separate always-visible line here to say *why* the dependency exists. */
export function implicitDepsForBlock(
  nodeId: string,
  envVarNames: string[],
  interpreterVarNames: string[],
  decls: Map<string, ClientVarDecl>,
): ImplicitDep[] {
  const seed = envVarNames.concat(interpreterVarNames);
  const out: ImplicitDep[] = [];
  for (const varName of closeOverVarRefs(decls, seed)) {
    const from = decls.get(varName)?.from;
    if (from) out.push({ varName, source: from });
  }
  return out;
}
