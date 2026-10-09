import assert from 'node:assert/strict';
import test from 'node:test';
import { retainLiveBlocks } from '../src/liveBlockSnapshot.ts';
const output = { status: 'done', text: 'visible output', exitCode: 0 };
const running = { status: 'running', text: 'partial output' };
const previous = { serverSession: 'worker', liveBlocks: { task: output, 'task[en]': running, removed: output } };
const node = { id: 'node', text: '```bash name="task"\necho hello\n```' };
test('edits retain finished and in-flight parameterized output without replay', () => {
  assert.deepEqual(retainLiveBlocks(node, 'worker', previous), { task: output, 'task[en]': running });
  assert.equal(retainLiveBlocks(node, 'worker', previous).task, output);
});
test('deleted blocks and restarted workers discard obsolete output', () => {
  assert.deepEqual(retainLiveBlocks({ ...node, text: 'prose' }, 'worker', previous), {});
  assert.deepEqual(retainLiveBlocks(node, 'new-worker', previous), {});
  assert.deepEqual(retainLiveBlocks(node, 'worker'), {});
});
test('file runs survive metadata edits', () => {
  assert.deepEqual(retainLiveBlocks({ id: 'task', type: 'file', text: '' }, 'worker', { serverSession: 'worker', liveBlocks: { task: output } }), { task: output });
});
