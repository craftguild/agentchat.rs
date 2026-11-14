import assert from 'node:assert/strict';
import { test } from 'node:test';
import { participatingAgents, retainedAgents } from '../../src/agents.ts';
import { agents as decodeAgents } from '../../src/validate.ts';

test('participants include silent joined agents, exclude left identities and drain bounded pages', async () => {
  const calls:(string | null)[] = [];
  const agents = await participatingAgents(async after => {
    calls.push(after);
    return after ? { items:[{kind:'AGENT',id:'2',handle:'silent',joined:true,host:'codex'}], nextCursor:null, scanCursor:'9', head:'9' }
      : { items:[{kind:'AGENT',id:'1',handle:'left',joined:false,host:'claude'}], nextCursor:'2', scanCursor:'2', head:'9' };
  }, new AbortController().signal);
  assert.deepEqual(calls, [null,'2']);
  assert.deepEqual(agents.map(agent => agent.handle), ['silent']);
});

test('participant paging rejects stalled cursors and cancelled requests', async () => {
  await assert.rejects(participatingAgents(async () => ({items:[],nextCursor:'0',scanCursor:'0',head:'9'}), new AbortController().signal));
  const controller = new AbortController(); controller.abort();
  await assert.rejects(participatingAgents(async () => ({items:[],nextCursor:null,scanCursor:'9',head:'9'}), controller.signal), {name:'AbortError'});
});


test('retained presentation directory includes departed hosts without joining them', async () => {
  const rows = [{kind:'AGENT',id:'left',handle:'alpha',joined:false,host:'claude'}, {kind:'AGENT',id:'live',handle:'beta',joined:true,host:'codex'}];
  const read = async () => ({items:rows,nextCursor:null,scanCursor:'2',head:'2'});
  assert.deepEqual(await retainedAgents(read,new AbortController().signal),rows);
  assert.deepEqual((await participatingAgents(read,new AbortController().signal)).map(agent => agent.id),['live']);
});

test('host kinds remain bounded typed text, including future kinds', () => {
  const page={items:[{kind:'AGENT',id:'00000000-0000-0000-0000-000000000003',handle:'alice',joined:true,host:'future-host'}],
    nextCursor:null,scanCursor:'1',head:'1'};
  assert.equal(decodeAgents(page).items[0]!.host,'future-host');
  assert.throws(() => decodeAgents({...page,items:[{...page.items[0],host:123}]}));
  assert.throws(() => decodeAgents({...page,items:[{...page.items[0],host:'x'.repeat(65)}]}));
});
