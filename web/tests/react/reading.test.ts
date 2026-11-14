import { test, expect } from 'vitest';
import { startReading,receiveReading,markReading,expireReading,nextExpiry,unreadCounts,unreadReplies,newestPost,sectionStart,MAX_TRACKING } from '../../src/state/reading.ts';
import { cp,message,batch } from './fixtures.ts';

test('the initial snapshot is baseline; exact visibility clears individual holes including replies',()=>{
  let state=startReading(cp(5));
  state=markReading(state,[message(3)],0);
  expect(unreadCounts(state).size).toBe(0);
  const first=message(6),reply=message(7,first.id),last=message(8);
  state=receiveReading(state,batch(5,first,reply,last));
  expect(unreadCounts(state).get('room')).toBe(3);
  state=markReading(state,[last],1);
  expect(unreadCounts(state).get('room')).toBe(2);
  expect(unreadReplies(state,first.id)).toBe(true);
  expect(newestPost(state,'room')).toBeNull();
  state=receiveReading(state,batch(5,first,reply,last));
  expect(unreadCounts(state).get('room')).toBe(2);
  state=markReading(state,[reply],2);
  expect(unreadReplies(state,first.id)).toBe(false);
  expect(unreadCounts(state).get('room')).toBe(1);
});
test('display before SSE is retained until its scan passes, even after viewed flags are evicted',()=>{
  let state=startReading(cp(0));state=markReading(state,[message(2)],0);
  expect(state.ahead.has(message(2).id)).toBe(true);
  state={...state,seen:new Map()};
  state=receiveReading(state,batch(0,message(1),message(2),message(3)));
  expect([...state.pending.keys()]).toEqual([message(1).id,message(3).id]);
  expect(state.ahead.size).toBe(0);
});
test('root and per-thread sections have independent absolute ten-second deadlines',()=>{
  const root=message(1),reply=message(2,root.id),other=message(3,'other');
  let state=receiveReading(startReading(cp(0)),batch(0,root,reply,other));
  state=markReading(state,[root,other],100);
  expect(nextExpiry(state)).toBe(10100);
  state=expireReading(state,10100);
  expect(sectionStart(state,'room',null)).toBeNull();
  expect(sectionStart(state,'room','other')).toBeNull();
  expect(sectionStart(state,'room',root.id)?.id).toBe(reply.id);
  state=markReading(state,[reply],11000);
  state=markReading(state,[reply],20999);
  expect(nextExpiry(state)).toBe(21000);
  expect(sectionStart(expireReading(state,21000),'room',root.id)).toBeNull();
});
test('new unseen arrivals cancel the completed cohort deadline; old replay cannot renew it',()=>{
  let state=receiveReading(startReading(cp(0)),batch(0,message(1)));
  state=markReading(state,[message(1)],0);
  state=receiveReading(state,batch(1,message(2),message(3)));
  expect(nextExpiry(state)).toBeNull();
  state=expireReading(state,100000);
  expect(sectionStart(state,'room',null)?.id).toBe(message(2).id);
  state=markReading(state,[message(2)],100000);
  expect(nextExpiry(state)).toBeNull();
  state=markReading(state,[message(3)],100001);
  state=receiveReading(state,batch(1,message(2),message(3)));
  expect(nextExpiry(state)).toBe(110001);
});
test('ordering uses BigInt sequence rather than timestamp and keeps channel identity',()=>{
  const first={...message(1),creationSeq:'9007199254740993'},last={...message(2),creationSeq:'9007199254740994',createdAt:'2000-01-01T00:00:00Z'};
  const state=markReading(startReading(cp('9223372036854775807')),[last,first,message(3,null,'other')],0);
  expect(state.viewed.get('room')?.id).toBe(last.id);
  expect(state.viewed.get('other')?.id).toBe(message(3).id);
});
test('overflow and a cursor gap reject the whole transition without mutating prior progress',()=>{
  const prior=startReading(cp(0));
  expect(()=>receiveReading(prior,batch(1,message(2)))).toThrow();
  expect(()=>receiveReading(prior,batch(0,...Array.from({length:MAX_TRACKING+1},(_,i)=>message(i+1))))).toThrow();
  expect(prior.scan.seq).toBe('0');expect(prior.pending.size).toBe(0);
  expect(()=>markReading(prior,Array.from({length:MAX_TRACKING+1},(_,i)=>message(i+1)),0)).toThrow();
  expect(prior.seen.size).toBe(0);
});
