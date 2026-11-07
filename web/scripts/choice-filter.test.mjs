import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import { describePending, filterChoices } from '../src/choiceFilter.ts';

// The same table `crates/core/src/vars.rs` runs against `filter_choices`:
// typing the same text narrows a select to the same entries in the browser,
// the TUI and the `meshfox` prompt.
const table = JSON.parse(
  fs.readFileSync(new URL('../../crates/core/testdata/choice_filter.json', import.meta.url), 'utf8'),
);

test('filterChoices matches the table shared with the Rust implementation', () => {
  for (const { query, expect } of table.cases) {
    assert.deepEqual(filterChoices(table.choices, query), expect, `query ${JSON.stringify(query)}`);
  }
});

test('describePending says why, or that the source has no value', () => {
  assert.equal(
    describePending({ var: 'FONT_LIST', reason: '`fonts/list` exited with status 3' }),
    'its choices come from FONT_LIST, which could not be computed: `fonts/list` exited with status 3',
  );
  assert.equal(describePending({ var: 'FONT_LIST' }), 'its choices come from FONT_LIST, which has no value yet');
});
