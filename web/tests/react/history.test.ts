import { test,expect } from 'vitest';
import { createViewerClient,createContentCache } from '../../src/graphql/client.ts';
import type { ViewerSdk } from '../../src/graphql/sdk.ts';
import { loadHistory,hydrateHistory } from '../../src/state/history.ts';
import { ingestTimeline,prependTimeline,emptyTimeline,metadata,targetTimeline,MAX_ROWS } from '../../src/state/timeline.ts';
import { cp,message,batch } from './fixtures.ts';
import type { Checkpoint } from '../../src/types.ts';
const signal=()=>new AbortController().signal;
function reads(rows=Array.from({length:120},(_,i)=>message(i+2)),head='121'){
  const calls:{before:string|null;snapshot:Checkpoint|null}[]=[];
  const roots=async(_channel:string,before:string|null,snapshot:Checkpoint|null,limit:number)=>{
    calls.push({before,snapshot});const through=snapshot??cp(head);
    if(before&&BigInt(before)>BigInt(through.seq))throw Error('Invalid cursor');
    const selected=rows.filter(m=>BigInt(m.creationSeq)<=BigInt(through.seq)&&(!before||BigInt(m.creationSeq)<BigInt(before)));
    const items=selected.slice(-Math.min(limit,2));
    return {snapshot:through,items,nextBefore:selected.length>items.length?items[0]!.creationSeq:null};
  };
  const sdk={
    Roots:async({channel,before,snapshot,limit})=>({roots:await roots(channel,before,snapshot,limit)}),
    Replies:async({channel,before,snapshot,limit})=>({replies:await roots(channel,before,snapshot,limit)}),
    Pinned:async({rootId,snapshot})=>({pinnedRoot:{snapshot,root:{...message(1),id:rootId,threadId:rootId}}}),
    Validate:async({through})=>({changes:{head:through}}),Changes:async()=>{throw Error('Unexpected replay');},
  } as unknown as ViewerSdk;
  return {sdk,calls};
}
test('latest/older pages drain bounded windows at one snapshot with no missing IDs',async()=>{
  const {sdk,calls}=reads(),cache=createContentCache(createViewerClient());
  let view=await loadHistory(sdk,cache,'dev',null,null,null,()=>cp(121),[],signal());
  expect(view.rows).toHaveLength(50);expect(view.rows[0]!.message.creationSeq).toBe('72');
  while(view.before)view=prependTimeline(view,await loadHistory(sdk,cache,'dev',null,view.before,view.snapshot,()=>cp(121),[],signal()));
  expect(view.rows).toHaveLength(120);expect(view.rows[0]!.message.creationSeq).toBe('2');
  expect(calls.slice(1).every(c=>c.snapshot?.seq==='121')).toBe(true);
});
for(const root of [null,'parent'])for(const head of ['121','9223372036854775807'])for(const atHead of [true,false]){
  test(`${root?'reply':'root'} eviction hydration ${atHead?'at':'below'} exact head ${head}`,async()=>{
    const highest=BigInt(head)-(atHead?0n:1n),rows=Array.from({length:5},(_,i)=>({...message(i+2,root),creationSeq:String(highest-4n+BigInt(i))}));
    const {sdk,calls}=reads(rows,head),client=createViewerClient(),cache=createContentCache(client,2000);
    const view=await loadHistory(sdk,cache,'dev',root,null,null,()=>cp(head),[],signal());
    const group=view.rows.filter(r=>BigInt(r.message.creationSeq)<=highest);
    for(let i=0;i<30;i++)cache.put(message(i+200));
    expect(group.every(r=>!cache.has(r.message.id))).toBe(true);
    calls.length=0;await hydrateHistory(sdk,cache,view,group,signal());
    expect(calls).toHaveLength(3);expect(calls[0]!.before).toBe(atHead?null:head);
    for(const row of group)expect(cache.get(row.message.id)?.body).toBe(rows.find(m=>m.id===row.message.id)!.body);
    await hydrateHistory(sdk,cache,view,group,signal());expect(calls).toHaveLength(3);
    expect(cache.bytes).toBeLessThanOrEqual(2000);
  });
}
test('reply counts are snapshot-specific, root order is stable and older projections cannot overwrite newer counts',()=>{
  let view={...emptyTimeline('dev',null,cp(10)),rows:[metadata({...message(1),replyCount:'3'},cp(10))]};
  const change=batch(10,{...message(11,message(1).id),replyCount:'4'});
  view=ingestTimeline(view,change);
  expect(view.rows.map(r=>r.message.id)).toEqual([message(1).id]);expect(view.rows[0]!.message.replyCount).toBe('4');
  view=prependTimeline(view,{...emptyTimeline('dev',null,cp(5)),rows:[metadata({...message(1),replyCount:'0'},cp(5))]});
  expect(view.rows[0]!.message.replyCount).toBe('4');
});
test('bounded history preserves the reader neighborhood and frozen intent while exposing a newer gap',()=>{
  let view={...emptyTimeline('dev',null,cp(MAX_ROWS)),rows:Array.from({length:MAX_ROWS},(_,i)=>metadata(message(i+1),cp(MAX_ROWS))),anchor:{id:message(1).id,offset:-12}};
  const state=ingestTimeline(view,batch(MAX_ROWS,message(MAX_ROWS+1)));
  expect(state.rows).toHaveLength(MAX_ROWS);expect(state.rows[0]!.message.id).toBe(message(1).id);expect(state.newerGap).toBe(true);
  const target=targetTimeline(state,message(MAX_ROWS+1).id);
  expect(ingestTimeline(target,batch(MAX_ROWS+1,message(MAX_ROWS+2))).target).toEqual(target.target);
  expect(view.rows).toHaveLength(MAX_ROWS);
});
