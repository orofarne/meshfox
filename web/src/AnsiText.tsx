import type { CSSProperties } from "react";
import Anser from "anser";

/**
 * Renders text that may contain ANSI SGR color/style escape codes (e.g.
 * from a script that colorizes its own output) as React elements — one
 * `<span>` per parsed chunk, never `dangerouslySetInnerHTML` (nothing else
 * in this codebase injects raw HTML; no reason to start for arbitrary
 * command output). Plain text with no ANSI codes comes back as a single
 * chunk, so this is safe to use unconditionally in place of a bare
 * `{text}` anywhere output renders.
 *
 * Caveat: commands run without a PTY (`Stdio::piped()`/`tokio::process` on
 * the server side, no pseudo-terminal allocated), so tools that
 * auto-detect "not a real terminal" and disable color on their own
 * (`cargo`, `git`, plain `ls`) will still print plain text here unless the
 * script itself forces color (`--color=always`, a raw
 * `echo -e '\033[31m...'`) — not a bug in this component, just how piped
 * subprocess output behaves everywhere.
 */
export function AnsiText({ text }: { text: string }) {
  const chunks = Anser.ansiToJson(text, { use_classes: false, remove_empty: true });
  return (
    <>
      {chunks.map((chunk, i) => {
        const style: CSSProperties = {};
        // anser's `fg`/`bg` (despite the .d.ts claiming plain `string`) are
        // bare "R, G, B" triples, e.g. "187, 0, 0" — not a valid CSS color
        // on their own, and can be `null` for "no color set" despite the
        // non-nullable type. Wrap in `rgb(...)` ourselves.
        if (chunk.fg) style.color = `rgb(${chunk.fg})`;
        if (chunk.bg) style.backgroundColor = `rgb(${chunk.bg})`;
        if (chunk.decorations.includes("bold")) style.fontWeight = "bold";
        if (chunk.decorations.includes("italic")) style.fontStyle = "italic";
        if (chunk.decorations.includes("underline")) style.textDecoration = "underline";
        if (chunk.decorations.includes("dim")) style.opacity = 0.7;
        if (chunk.decorations.includes("strikethrough")) {
          style.textDecoration = style.textDecoration ? `${style.textDecoration} line-through` : "line-through";
        }
        return (
          <span key={i} style={Object.keys(style).length > 0 ? style : undefined}>
            {chunk.content}
          </span>
        );
      })}
    </>
  );
}
