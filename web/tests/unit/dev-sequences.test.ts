import assert from 'node:assert/strict';
import {test} from 'node:test';
import {fixtures} from '../../scripts/dev-project.mjs';
import {batch} from '../../src/validate.ts';

test('Mock seed shares causal ordering while keeping audit gaps and the 160-event budget',()=>{
  const seed=fixtures(), changes=seed.Change, messages=seed.Message;
  const eventIds=new Set(changes.map(event=>event.seq));
  assert.equal(seed.DevEventBudget[0]!.events,changes.length);
  assert(changes.length<160);
  assert.equal(eventIds.size,changes.length);
  for(const message of messages){
    const event=changes.find(change=>change.message?.id===message.id)!;
    assert(BigInt(message.creationSeq)<BigInt(event.seq));
    assert(!eventIds.has(message.creationSeq));
  }
  const head=seed.Project[0]!.checkpoint;
  assert.equal(head.seq,changes.at(-1)!.seq);
  // Query/SSE validators accept the sparse audit stream, never the equal-seq legacy shape.
  assert.equal(batch({after:{...head,seq:'0',eventId:null},scan:head,head,hasMore:false,items:changes}).items.length,changes.length);
});
