import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { chmod, mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { pubsubSelector, snapshotCli } from '../../scripts/dev-compat.mjs';

function declaration(subscribe: string, publish = subscribe): string {
  return `export type Setup = { ["pubsub"]: { ["subscribe"]: {
    ["${subscribe}"]: (input: PubSubInput) -> Source,
  } } }
  export type Mutation = { ["pubsub"]: { ["publish"]: {
    ["${publish}"]: (input: EventInput) -> (),
  } } }`;
}

test('PubSub wiring follows generated model-scoped and field-scoped selectors', () => {
  for (const selector of ['ChangeBatch', 'changes']) {
    const source = declaration(selector);
    assert.equal(pubsubSelector(source + source), JSON.stringify(selector));
  }
});

test('missing, ambiguous and mixed generated PubSub surfaces fail explicitly', () => {
  for (const source of ['', declaration('ChangeBatch', 'changes'), declaration('changes') + declaration('Other')]) {
    assert.throws(() => pubsubSelector(source), /Unsupported generated Mock PubSub contract/);
  }
});

test('an original CLI replacement cannot change the per-launch executable snapshot', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'agentchat-cli-snapshot-test-'));
  try {
    const original = join(directory, 'source'), owned = join(directory, 'owned');
    await mkdir(owned);
    await writeFile(original, '#!/bin/sh\nprintf "first"\n'); await chmod(original, 0o700);
    const selected = await snapshotCli(original, owned);
    await writeFile(original, '#!/bin/sh\nprintf "second"\n');
    assert.equal(execFileSync(selected.path, {encoding:'utf8'}), 'first');
    assert.equal(execFileSync(original, {encoding:'utf8'}), 'second');
    assert.match(selected.sha256, /^[a-f0-9]{64}$/);
  } finally {await rm(directory, {recursive:true, force:true});}
});
