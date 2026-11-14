import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { workspaceVersion, writeBuildInfo } from '../../scripts/build-info.mjs';
import { buildLabel } from '../../src/build-info.ts';

test('product version comes only from the Cargo workspace package', () => {
  assert.equal(workspaceVersion('[package]\nversion="9.9.9"\n[workspace.package]\nversion="0.1.0"\n[workspace.dependencies]\nversion="8.8.8"'), '0.1.0');
  assert.throws(() => workspaceVersion('[package]\nversion="9.9.9"'));
  assert.throws(() => workspaceVersion('[workspace.package]\nversion="not-a-version"'));
});

test('source archives get truthful unknown revisions and need no runtime source access', async () => {
  const root = await mkdtemp(join(tmpdir(), 'agentchat-build-info-'));
  try {
    await writeFile(join(root, 'Cargo.toml'), '[workspace.package]\nversion="0.2.0"\n');
    await writeBuildInfo(root);
    const info = JSON.parse(await readFile(join(root, 'web/public/build-info.txt'), 'utf8'));
    assert.deepEqual(info, {version:'0.2.0',revision:'unknown'});
    assert.equal(buildLabel(info), 'v0.2.0 (unknown)');
  } finally { await rm(root, {recursive:true,force:true}); }
});

test('footer validates metadata and formats a seven-digit source revision', () => {
  assert.equal(buildLabel({version:'0.1.0',revision:'1234567890abcdef'.repeat(2)+'12345678'}), 'v0.1.0 (1234567)');
  for (const info of [null,{}, {version:'0.1.0',revision:'fake'}, {version:'invalid',revision:'unknown'}]) {
    assert.throws(() => buildLabel(info));
  }
});
