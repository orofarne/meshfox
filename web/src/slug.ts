/**
 * Client-side slug helpers for suggesting a node id from its title (see
 * `NodeSettings.tsx`). Independent of the server's own auto-slug
 * (`crates/core/src/canvas.rs::slugify`, used when a node is first created
 * or parsed with no explicit id) — that one deliberately doesn't
 * transliterate non-Latin scripts, so `basicSlug` below mirrors it exactly
 * (same algorithm, same non-transliterating behavior) purely to detect
 * "this id still matches what the server would have generated from the
 * title", while `translitSlug` is the richer one actually offered as a
 * suggestion.
 */

/** Mirrors `crates/core/src/canvas.rs::slugify` — lowercases, collapses
 * every run of non-alphanumeric characters to a single `-`, trims leading
 * and trailing `-`. No transliteration: non-Latin letters pass through
 * lowercased, same as the Rust original. */
export function basicSlug(s: string): string {
  let out = "";
  let prevDash = false;
  for (const ch of s.toLowerCase()) {
    if (/[\p{L}\p{N}]/u.test(ch)) {
      out += ch;
      prevDash = false;
    } else if (!prevDash && out.length > 0) {
      out += "-";
      prevDash = true;
    }
  }
  return out.replace(/-+$/, "");
}

// Practical (not ISO 9) Cyrillic → Latin transliteration table — reads
// naturally rather than being reversible, same spirit as the id itself
// (a human-facing handle, not a stored identity that needs round-tripping
// back to Cyrillic).
const CYRILLIC: Record<string, string> = {
  а: "a", б: "b", в: "v", г: "g", д: "d", е: "e", ё: "e", ж: "zh", з: "z", и: "i",
  й: "y", к: "k", л: "l", м: "m", н: "n", о: "o", п: "p", р: "r", с: "s", т: "t",
  у: "u", ф: "f", х: "kh", ц: "ts", ч: "ch", ш: "sh", щ: "shch", ъ: "", ы: "y",
  ь: "", э: "e", ю: "yu", я: "ya",
};

function transliterate(s: string): string {
  let out = "";
  for (const ch of s.toLowerCase()) {
    out += CYRILLIC[ch] ?? ch;
  }
  return out;
}

/** The id suggested from a title: transliterates Cyrillic to Latin first
 * (see `CYRILLIC`), then applies the same slugging rules as `basicSlug`. */
export function translitSlug(s: string): string {
  return basicSlug(transliterate(s));
}
