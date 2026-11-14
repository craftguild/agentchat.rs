import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import {liveChanges} from '../../src/graphql/live.ts';
import {createSdk} from '../../src/graphql/sdk.ts';
import {createViewerClient,createContentCache,BODY_BYTES} from '../../src/graphql/client.ts';
import type {ContentCache} from '../../src/graphql/client.ts';
import {loadHistory,hydrateHistory} from '../../src/state/history.ts';
import {ingestTimeline} from '../../src/state/timeline.ts';
const api=createSdk(createViewerClient());
const subscribe=(after,signal)=>liveChanges(api,after,signal);
import type { Row } from '../../src/state/timeline.ts';
import type { Message, WireError } from '../../src/types.ts';
import { Provider } from '../browser/provider.ts';

function evict(bodies: ContentCache, message: Message): void {
  const body = 'x'.repeat(60_000), bodyHtml = `<p>${'x'.repeat(250_000)}</p>`;
  for (let n = 0; n < 120; n++) bodies.put({ ...message, body, bodyHtml,
    id: `00000000-0000-0000-0000-${String(1000 + n).padStart(12, '0')}` });
  assert(!bodies.has(message.id), 'LRU pressure must actually evict the selected body.');
  assert(bodies.bytes <= BODY_BYTES);
}

test('real CLI/SQLite hydrates evicted roots and replies within snapshots and continues live delivery',
  { timeout: 120_000 }, async () => {
    const executable = process.env.AGENTCHAT_REAL_BROWSER_CLI_BINARY;
    assert(executable && process.env.AGENTCHAT_REAL_BROWSER_BINARY,
      'Build and select the same real provider/CLI images used by the browser integration suite.');
    const provider = new Provider('real'), originalFetch = globalThis.fetch;
    const abort = new AbortController(), signal = abort.signal;
    try {
      await provider.start();
      globalThis.fetch = (input, init) => originalFetch(typeof input === 'string' ? new URL(input, provider.url) : input, init);
      const initial = await provider.control({ command: 'status' });
      assert.equal(initial.cli_image_sha256, createHash('sha256').update(readFileSync(executable)).digest('hex'));
      for (const root of [null, initial.thread_root as string]) {
        const body = root ? 'Reply at hydration snapshot 日本語' : 'Root at hydration snapshot 日本語';
        await provider.control(root ? { command: 'reply', root, body } : { command: 'root', body });
        const project = (await api.Project(undefined,{signal})).project, bodies = createContentCache(createViewerClient());
        let pane=await loadHistory(api,bodies,'dev',root,null,null,()=>project.checkpoint,[],signal);
        const atHead = pane.rows.at(-1)!;
        assert(BigInt(atHead.message.creationSeq) < BigInt(atHead.snapshot.seq));
        assert.equal(bodies.get(atHead.message.id)?.body, body);
        const invalid = (BigInt(atHead.snapshot.seq) + 1n).toString();
        const invalidWindow = root ? api.Replies({channel:'dev',rootId:root,before:invalid,snapshot:atHead.snapshot,limit:1},{signal})
          : api.Roots({channel:'dev',before:invalid,snapshot:atHead.snapshot,limit:1},{signal});
        await assert.rejects(invalidWindow, (error: WireError) => error.code === 'invalid_argument');
        const belowHead = pane.rows.at(-2)!;
        assert(BigInt(belowHead.message.creationSeq) < BigInt(belowHead.snapshot.seq));
        const expectedBelow = bodies.get(belowHead.message.id)!.body;
        for (const [row, expected] of [[atHead, body], [belowHead, expectedBelow]] as [Row, string][]) {
          evict(bodies, { ...row.message, body: expected, bodyHtml: '<p>evicted</p>', renderFallback: false });
          await hydrateHistory(api,bodies,pane,[row],signal);
          assert.equal(bodies.get(row.message.id)?.body, expected);
        }
        if (pane.pinned) {
          const pinned = pane.pinned;
          await hydrateHistory(api,bodies,pane,[pinned],signal);
          assert(bodies.has(pinned.message.id), 'Separate pinned-root recovery must remain usable.');
        }
        const controller = new AbortController(), source = subscribe(project.checkpoint, controller.signal);
        try {
          let next = source.next();
          const liveBody = root ? 'Reply after successful hydration' : 'Root after successful hydration';
          await provider.control(root ? { command: 'reply', root, body: liveBody } : { command: 'root', body: liveBody });
          let found = false;
          for (let frame = 0; !found && frame < 4; frame++) {
            const result = await next;
            assert(!result.done);
            for(const item of result.value.items)if(item.kind==='MESSAGE_CREATED')bodies.put(item.message);
            pane=ingestTimeline(pane,result.value);
            const changed = result.value.items.find(item => item.message?.body === liveBody);
            if (changed?.message) {
              assert.equal(bodies.get(changed.message.id)?.body, liveBody);
              assert.equal(pane.applied.seq, result.value.scan.seq);
              found = true;
            } else next = source.next();
          }
          assert(found, 'The actual project Subscription must continue after body recovery.');
        } finally { controller.abort(); await source.return(undefined); }
      }
      const stopped = await provider.control({ command: 'stop_viewer' });
      assert.equal(stopped.runtime_instance, initial.runtime_instance);
      assert.equal(stopped.author_id, initial.author_id);
      assert.equal(stopped.observers, 0);
      assert.equal(stopped.statistics.messages, initial.statistics.messages + 4);
      assert.equal(stopped.statistics.read_receipts, 0);
      assert.equal(Object.hasOwn(stopped.statistics, 'acknowledged_deliveries'), false);
    } finally {
      abort.abort(); globalThis.fetch = originalFetch;
      await provider.close();
    }
  });
