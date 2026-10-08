import assert from 'node:assert/strict';
import test from 'node:test';
import {
  BLOCK_ROWS, MAX_TRACK_PX, ROW_H, blocksFor, buildFilters, firstRowAt, formatBytes,
  parseFilterExpression, scrollRatio, scrollTopForRow, toggleSort, trackHeight, viewKey,
  visibleRowCount, isEmptyView,
} from '../src/tableView.ts';

test('filter expressions: operators, defaults by column kind, null checks', () => {
  assert.deepEqual(parseFilterExpression('>= 10', 'number'), { op: 'ge', value: '10' });
  assert.deepEqual(parseFilterExpression('<5', 'number'), { op: 'lt', value: '5' });
  assert.deepEqual(parseFilterExpression('!=x', 'text'), { op: 'ne', value: 'x' });
  assert.deepEqual(parseFilterExpression('^ab', 'text'), { op: 'startswith', value: 'ab' });
  assert.deepEqual(parseFilterExpression('$ab', 'text'), { op: 'endswith', value: 'ab' });
  assert.deepEqual(parseFilterExpression('~ab', 'number'), { op: 'contains', value: 'ab' });
  assert.deepEqual(parseFilterExpression('42', 'number'), { op: 'eq', value: '42' });
  assert.deepEqual(parseFilterExpression('true', 'bool'), { op: 'eq', value: 'true' });
  assert.deepEqual(parseFilterExpression('2024-01', 'temporal'), { op: 'contains', value: '2024-01' });
  assert.deepEqual(parseFilterExpression('foo', 'text'), { op: 'contains', value: 'foo' });
  assert.deepEqual(parseFilterExpression('NULL', 'text'), { op: 'isnull' });
  assert.deepEqual(parseFilterExpression('!null', 'number'), { op: 'notnull' });
});

test('an empty filter or an operator without a value is ignored', () => {
  assert.equal(parseFilterExpression('   ', 'text'), null);
  assert.equal(parseFilterExpression('>', 'number'), null);
  assert.equal(parseFilterExpression('>=  ', 'number'), null);
});

test('filters are built per column in column order, skipping blanks', () => {
  const columns = [
    { index: 0, name: 'id', type: 'BIGINT', kind: 'number' },
    { index: 1, name: 'name', type: 'VARCHAR', kind: 'text' },
    { index: 2, name: 'note', type: 'VARCHAR', kind: 'text' },
  ];
  assert.deepEqual(buildFilters({ 1: 'ab', 0: '>3', 2: '' }, columns), [
    { column: 0, op: 'gt', value: '3' },
    { column: 1, op: 'contains', value: 'ab' },
  ]);
});

test('sort cycles none → asc → desc → none; shift adds secondary keys', () => {
  let s = toggleSort([], 2, false);
  assert.deepEqual(s, [{ column: 2, desc: false }]);
  s = toggleSort(s, 2, false);
  assert.deepEqual(s, [{ column: 2, desc: true }]);
  s = toggleSort(s, 2, false);
  assert.deepEqual(s, []);

  // A plain click on another column replaces the keys.
  s = toggleSort([{ column: 0, desc: false }, { column: 1, desc: true }], 3, false);
  assert.deepEqual(s, [{ column: 3, desc: false }]);

  // Shift keeps them, cycling only the clicked one.
  s = toggleSort([{ column: 0, desc: false }], 1, true);
  assert.deepEqual(s, [{ column: 0, desc: false }, { column: 1, desc: false }]);
  s = toggleSort(s, 0, true);
  assert.deepEqual(s, [{ column: 0, desc: true }, { column: 1, desc: false }]);
  s = toggleSort(s, 0, true);
  assert.deepEqual(s, [{ column: 1, desc: false }]);
});

test('view identity ignores a blank search and detects the empty view', () => {
  assert.equal(viewKey({ sort: [], filters: [], search: '' }), viewKey({ sort: [], filters: [] }));
  assert.notEqual(viewKey({ sort: [], filters: [], search: 'x' }), viewKey({ sort: [], filters: [] }));
  assert.ok(isEmptyView({ sort: [], filters: [], search: '' }));
  assert.ok(!isEmptyView({ sort: [], filters: [], search: 'x' }));
});

test('small tables scroll 1:1', () => {
  const rows = 1000;
  const viewport = 400;
  assert.equal(trackHeight(rows), rows * ROW_H);
  assert.equal(scrollRatio(rows, viewport), 1);
  assert.deepEqual(firstRowAt(0, rows, viewport), { row: 0, offsetPx: 0 });
  assert.deepEqual(firstRowAt(ROW_H * 10 + 5, rows, viewport), { row: 10, offsetPx: 5 });
  assert.equal(scrollTopForRow(10, rows, viewport), ROW_H * 10);
});

test('huge tables get a capped track and still reach every row', () => {
  const rows = 100_000_000;
  const viewport = 500;
  assert.equal(trackHeight(rows), MAX_TRACK_PX);
  const ratio = scrollRatio(rows, viewport);
  assert.ok(ratio > 100);

  // Top and bottom of the track are the first and last rows.
  assert.equal(firstRowAt(0, rows, viewport).row, 0);
  const maxScroll = MAX_TRACK_PX - viewport;
  const last = firstRowAt(maxScroll, rows, viewport);
  const lastVisibleY = last.row * ROW_H + last.offsetPx + viewport;
  assert.ok(Math.abs(lastVisibleY - rows * ROW_H) < ROW_H, 'bottom of the scroll is the bottom of the data');

  // The two mappings are inverse (to within a row).
  for (const row of [0, 1, 12_345, 50_000_000, 99_999_000]) {
    const back = firstRowAt(scrollTopForRow(row, rows, viewport), rows, viewport).row;
    assert.ok(Math.abs(back - row) <= 1, `${row} → ${back}`);
  }
});

test('a short table never scrolls past its data', () => {
  assert.deepEqual(firstRowAt(10_000, 3, 400), { row: 2, offsetPx: 10_000 * 1 - 2 * ROW_H });
  assert.deepEqual(firstRowAt(50, 0, 400), { row: 0, offsetPx: 0 });
});

test('blocks cover the visible rows plus a margin and stay in range', () => {
  const rows = 1000;
  const count = visibleRowCount(400);
  assert.deepEqual(blocksFor(0, count, rows, 0), [0]);
  assert.deepEqual(blocksFor(0, count, rows, 1), [0, 1]);
  assert.deepEqual(blocksFor(BLOCK_ROWS * 5 + 90, count, rows, 1), [4, 5, 6, 7]);
  assert.deepEqual(blocksFor(990, count, rows, 1), [8, 9]);
  assert.deepEqual(blocksFor(0, count, 0, 1), []);
});

test('byte sizes read naturally', () => {
  assert.equal(formatBytes(512), '512 B');
  assert.equal(formatBytes(2048), '2.0 KiB');
  assert.equal(formatBytes(5 * 1024 * 1024 * 1024), '5.0 GiB');
});
