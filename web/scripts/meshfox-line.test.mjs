import assert from 'node:assert/strict';
import test from 'node:test';
import { colorMeshfoxLine } from '../src/meshfoxLine.ts';

const meta = (event) => ({ canvas: 'child.canvas.md', port: 1, event });

test('a relayed step banner is colored like `meshfox run`', () => {
  assert.equal(
    colorMeshfoxLine('==> child.canvas.md → build', meta({ type: 'step-start', block: 'build' })),
    '\x1b[1;36m==> child.canvas.md → build\x1b[0m',
  );
  assert.equal(
    colorMeshfoxLine('==> build (skipped)', meta({ type: 'step-skipped', block: 'build' })),
    '\x1b[1;36m==> build (skipped)\x1b[0m',
  );
});

test('a relayed step result is green on success and red on failure', () => {
  assert.equal(
    colorMeshfoxLine('<== build (exit 0)', meta({ type: 'step-end', exitCode: 0 })),
    '\x1b[32m<== build (exit 0)\x1b[0m',
  );
  assert.equal(
    colorMeshfoxLine('<== build (exit 7)', meta({ type: 'step-end', exitCode: 7 })),
    '\x1b[31m<== build (exit 7)\x1b[0m',
  );
});

test('the innermost event of a nested call decides', () => {
  const nested = meta({ type: 'output', child: meta({ type: 'step-start', block: 'x' }) });
  assert.equal(colorMeshfoxLine('==> a → x', nested), '\x1b[1;36m==> a → x\x1b[0m');
});

test('program output and lines that already carry colors are left alone', () => {
  assert.equal(colorMeshfoxLine('added 177 packages', meta({ type: 'output' })), 'added 177 packages');
  assert.equal(colorMeshfoxLine('plain', undefined), 'plain');
  assert.equal(
    colorMeshfoxLine('\x1b[31mred', meta({ type: 'step-start' })),
    '\x1b[31mred',
  );
  assert.equal(colorMeshfoxLine('', meta({ type: 'step-start' })), '');
});
