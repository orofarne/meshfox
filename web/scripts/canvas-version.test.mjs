import assert from 'node:assert/strict';
import test from 'node:test';
import { canvasVersionGate } from '../src/canvasVersion.ts';
const snapshot = (serverSession, canvasVersion) => ({ nodes: [], serverSession, canvasVersion });
test('late reads cannot undo an acknowledged mutation', () => {
  const accept = canvasVersionGate();
  assert.equal(accept(snapshot('one', 3)), true);
  assert.equal(accept(snapshot('one', 5)), true);
  assert.equal(accept(snapshot('one', 4)), false);
  assert.equal(accept(snapshot('one', 5)), true);
});
test('a worker restart accepts its reset counter but retires old responses', () => {
  const accept = canvasVersionGate();
  assert.equal(accept(snapshot('one', 100)), true);
  assert.equal(accept(snapshot('two', 1)), true);
  assert.equal(accept(snapshot('one', 101)), false);
  assert.equal(accept(snapshot('two', 2)), true);
  assert.equal(accept({ nodes: [] }), false);
});
