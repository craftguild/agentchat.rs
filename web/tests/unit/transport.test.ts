import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { events, MAX_FRAME_BYTES } from '../../src/sse.ts';
import * as decode from '../../src/validate.ts';
import { compare, WireError } from '../../src/types.ts';

const fixture = JSON.parse(readFileSync(new URL('../../fixtures/contract-v1.json', import.meta.url), 'utf8'));
const encoder = new TextEncoder();

function source(chunks: Uint8Array[]): ReadableStream<Uint8Array> {
  return new ReadableStream({ start(controller) { for (const chunk of chunks) controller.enqueue(chunk); controller.close(); } });
}
async function parsed(chunks: Uint8Array[]) {
  const rows = [];
  for await (const row of events(source(chunks), new AbortController().signal)) rows.push(row);
  return rows;
}

test('actual generated fixture decodes all expected typed projections', () => {
  assert.equal(decode.project(fixture.project).id, fixture.project.id);
  assert.equal(decode.channels(fixture.channels).items[0]!.name, 'dev');
  assert.equal(decode.window(fixture.roots).items[0]!.body, 'Hello **AgentChat**.');
  assert.deepEqual(decode.batch(fixture.changes), fixture.changes);
  assert.equal(compare('9007199254740993', '9007199254740992'), 1);
  assert.equal(compare('9223372036854775807', '9007199254740993'), 1);
});

test('UTF-8/CRLF/data boundaries may split at every byte; heartbeats never yield progress', async () => {
  const payload = { text: '💜 日本語', seq: '9007199254740993' };
  const bytes = encoder.encode(`: heartbeat\r\n\r\nevent: next\r\ndata: ${JSON.stringify(payload)}\r\n\r\nevent: complete\r\n\r\n`);
  for (let cut = 0; cut <= bytes.length; cut++) {
    assert.deepEqual(await parsed([bytes.slice(0, cut), bytes.slice(cut)]), [
      { event: 'next', data: payload }, { event: 'complete', data: null },
    ]);
  }
  assert.deepEqual(await parsed(Array.from(bytes, byte => Uint8Array.of(byte))), [
    { event: 'next', data: payload }, { event: 'complete', data: null },
  ]);
});

test('several legal large events in one chunk do not overflow the unfinished-frame buffer', async () => {
  const data = { body: 'x'.repeat(600_000) };
  const frame = `event: next\ndata: ${JSON.stringify(data)}\n\n`;
  const rows = await parsed([encoder.encode(frame.repeat(3) + 'event: complete\n\n')]);
  assert.equal(rows.length, 4);
  assert.deepEqual(rows[2], { event: 'next', data });
});

test('partial EOF, malformed UTF-8/JSON and oversized frames cannot yield a complete update', async () => {
  for (const bytes of [encoder.encode('event: next\ndata: {"seq":"2"}'),
    encoder.encode('event: next\ndata: {oops}\n\n'), Uint8Array.of(0xff),
    encoder.encode('event: next\ndata: ' + 'x'.repeat(MAX_FRAME_BYTES) + '\n\n')]) {
    let applied = 0;
    await assert.rejects(async () => {
      for await (const frame of events(source([bytes]), new AbortController().signal)) {
        if (frame.event === 'next') applied++;
      }
    });
    assert.equal(applied, 0);
  }
});

test('cancellation releases the owned stream after complete frames and never completes a partial one', async () => {
  const controller = new AbortController(); let cancelled = false;
  const stream = new ReadableStream<Uint8Array>({
    start(out) { out.enqueue(encoder.encode('event: next\ndata: {}\n\nevent: next\ndata: {')); },
    cancel() { cancelled = true; },
  });
  const iterator = events(stream, controller.signal);
  assert.deepEqual((await iterator.next()).value, { event: 'next', data: {} });
  controller.abort();
  await assert.rejects(iterator.next());
  assert.equal(cancelled, true);
});

test('range, project, string sequence and monotonic item guards reject corrupt complete payloads', () => {
  for (const mutate of [
    (v: any) => { v.scan.seq = 2; },
    (v: any) => { v.scan.projectId = '00000000-0000-0000-0000-000000000099'; },
    (v: any) => { v.after.seq = '3'; v.after.eventId = v.head.eventId; },
    (v: any) => { v.hasMore = true; },
    (v: any) => { v.items.reverse(); },
    (v: any) => { v.items[1].message.creationSeq = v.items[1].seq; },
  ]) {
    const value = structuredClone(fixture.changes); mutate(value);
    assert.throws(() => decode.batch(value), WireError);
  }
});


test('Message Header precedes its audit event while channel creation keeps equality', () => {
  const value = structuredClone(fixture.changes);
  assert.equal(decode.batch(value).items[1]!.message!.creationSeq, '2');
  for (const seq of ['3', '4']) {
    const wrong = structuredClone(value); wrong.items[1].message.creationSeq = seq;
    assert.throws(() => decode.batch(wrong), WireError);
  }
  const channel = structuredClone(value); channel.items[0].channel.creationSeq = '2';
  assert.throws(() => decode.batch(channel), WireError);
});
