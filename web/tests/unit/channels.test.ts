import assert from 'node:assert/strict';
import { test } from 'node:test';
import { allChannels } from '../../src/channels.ts';
import type { ChannelPage, ChannelView } from '../../src/types.ts';

const channel = (n: number): ChannelView => ({ id:String(n), name:`channel-${n}`, creationSeq:String(n),
  lastConversationAt:'2026-09-26T00:00:00.001Z', archiveAt:'2026-10-26T00:00:00.001Z', archived:false, mandatory:n === 1 });
const page = (value:Omit<ChannelPage,'asOf'>):ChannelPage => ({...value,asOf:'2026-10-26T00:00:00.000Z'});
test('all retained channels drain bounded pages through one head without chasing later additions', async () => {
  const requests: (string | null)[] = [];
  const result = await allChannels(async after => {
    requests.push(after);
    const start = after ? Number(after) + 1 : 1;
    const items = Array.from({length:50}, (_, n) => channel(start + n));
    return page({ items, nextCursor:String(start + 49), scanCursor:String(start + 49), head:after ? '200' : '125' });
  }, new AbortController().signal);
  assert.deepEqual(requests, [null, '50', '100']);
  assert.deepEqual(result.items.map(row => row.id), Array.from({length:125}, (_, n) => String(n + 1)));
  assert.equal(result.asOf, '2026-10-26T00:00:00.000Z');
});

test('channel enumeration rejects a replaced head or non-progressing continuation', async () => {
  for (const invalid of [
    page({ items:[], nextCursor:'1', scanCursor:'1', head:'0' }),
    page({ items:[], nextCursor:'1', scanCursor:'1', head:'2' }),
  ] satisfies ChannelPage[]) {
    let calls = 0;
    await assert.rejects(allChannels(async () => ++calls === 1
      ? page({ items:[channel(1)], nextCursor:'1', scanCursor:'1', head:'2' }) : invalid,
    new AbortController().signal));
    assert.equal(calls, 2);
  }
});

test('the last captured server time normalizes channels that age during paging', async () => {
  let call = 0;
  const result = await allChannels(async () => ++call === 1
    ? page({items:[channel(1)],nextCursor:'1',scanCursor:'1',head:'2'})
    : {...page({items:[channel(2)],nextCursor:null,scanCursor:'2',head:'2'}),asOf:'2026-10-26T00:00:00.001Z'},
  new AbortController().signal);
  assert.deepEqual(result.items.map(row => row.archived), [true,true]);
});
