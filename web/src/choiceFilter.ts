/**
 * Indices into `choices` of the entries that contain `query`, ignoring case
 * and surrounding whitespace — the filter every front end's `select` picker
 * uses (the TUI picker and the `meshfox` prompt run
 * `meshfox_core::filter_choices`, the same rule). The table both sides are
 * tested against is `crates/core/testdata/choice_filter.json`; an empty
 * query keeps every entry, in the declared order.
 */
export function filterChoices(choices: readonly string[], query: string): number[] {
  const needle = query.trim().toLowerCase();
  const kept: number[] = [];
  choices.forEach((choice, i) => {
    if (choice.toLowerCase().includes(needle)) kept.push(i);
  });
  return kept;
}

/** The sentence shown in place of an empty list of choices — the same
 * wording the terminal front ends use (`describe_pending` in the CLI). */
export function describePending(pending: { var: string; reason?: string }): string {
  return pending.reason
    ? `its choices come from ${pending.var}, which could not be computed: ${pending.reason}`
    : `its choices come from ${pending.var}, which has no value yet`;
}
