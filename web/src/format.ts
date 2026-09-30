/** Mirrors `core::output::format_duration_ms` — kept in sync by hand (same
 * split this codebase already uses for every other Rust/TS syntax pair, see
 * SPEC.md's "Formal grammar" intro): `"842ms"` under a second, `"2.3s"`
 * under a minute, `"1m 05s"` beyond that. */
export function formatDurationMs(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  const totalSeconds = Math.round(ms / 1000);
  if (totalSeconds < 60) return `${(ms / 1000).toFixed(1)}s`;
  return `${Math.floor(totalSeconds / 60)}m ${String(totalSeconds % 60).padStart(2, "0")}s`;
}
