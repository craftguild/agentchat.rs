import type { ViewerSdk } from '../graphql/sdk.ts';
import type { ContentCache } from '../graphql/client.ts';
import type { ChangeBatch, Checkpoint } from '../types.ts';
import * as decode from '../validate.ts';
import { compare, same, WireError } from '../types.ts';
import type { Row, Timeline } from './timeline.ts';
import { emptyTimeline, metadata, ingestTimeline, targetTimeline } from './timeline.ts';

/** Stateless history service. React owns view state; Apollo owns complete content. */
export async function loadHistory(sdk:ViewerSdk,contents:ContentCache,channel:string,root:string|null,
  before:string|null,snapshot:Checkpoint|null,progress:()=>Checkpoint,pending:ChangeBatch[],signal:AbortSignal):Promise<Timeline> {
  const start=progress(),latest=before===null;let state:Timeline|null=null;
  for(let read=0;read<50;read++){
    const page=root ? await sdk.Replies({channel,rootId:root,before,snapshot,limit:50-(state?.rows.length??0)},{signal}).then(data=>data.replies)
      : await sdk.Roots({channel,before,snapshot,limit:50-(state?.rows.length??0)},{signal}).then(data=>data.roots);
    signal.throwIfAborted();
    if(snapshot && !same(snapshot,page.snapshot))throw new WireError('Window snapshot changed.','changed_snapshot',true);
    if(before && page.items.some(m=>compare(m.creationSeq,before!)>=0))throw new WireError('History paging overlapped.','page_gap',true);
    snapshot=page.snapshot;state ??= emptyTimeline(channel,root,snapshot);
    for(const message of page.items)contents.put(message);
    state.rows.unshift(...page.items.map(m=>metadata(m,page.snapshot)));state.before=page.nextBefore;
    if(!page.nextBefore || state.rows.length>=50)break;
    if(!page.items.length || (before && compare(page.nextBefore,before)>=0))throw new WireError('History paging made no progress.','page_gap',true);
    before=page.nextBefore;if(read===49)throw new WireError('History paging exceeded its budget.','page_overflow',true);
  }
  if(!state)throw new Error('Missing history snapshot');
  if(state.snapshot.projectId!==start.projectId || (latest && compare(state.snapshot.seq,start.seq)<0)
    || (state.snapshot.seq===start.seq && !same(state.snapshot,start)))throw new WireError('History changed.','snapshot_changed',true);
  if(root){const parent=(await sdk.Pinned({channel,rootId:root,snapshot:state.snapshot},{signal})).pinnedRoot;if(!same(parent.snapshot,state.snapshot))throw new WireError('Thread snapshot changed.','changed_snapshot',true);contents.put(parent.root);state.pinned=metadata(parent.root,parent.snapshot);}
  const through=progress();
  if(compare(start.seq,state.snapshot.seq)<0)await sdk.Validate({after:start,through:state.snapshot},{signal});
  while(compare(state.applied.seq,through.seq)<0){
    const batch=decode.batch((await sdk.Changes({after:state.applied,through},{signal})).changes);
    if(compare(batch.scan.seq,state.applied.seq)<=0)throw new WireError('Replay made no progress.','replay_gap',true);
    for(const item of batch.items)if(item.kind==='MESSAGE_CREATED')contents.put(item.message);
    state=ingestTimeline(state,batch);
  }
  for(const batch of pending)state=ingestTimeline(state,batch);
  if(latest && state.rows.length>50){state.rows=state.rows.slice(-50);state.before=state.rows[0]!.message.creationSeq;}
  const last=state.rows.at(-1) ?? state.pinned;
  return last ? targetTimeline(state,last.message.id) : state;
}

/** Recover immutable bodies at their retained snapshot, including its exact head. */
export async function hydrateHistory(sdk:ViewerSdk,contents:ContentCache,view:Timeline,rows:Row[],signal:AbortSignal):Promise<void> {
  for(let index=0;index<rows.length;){
    const first=rows[index++]!,group=[first];
    if(first!==view.pinned)while(index<rows.length && rows[index]!==view.pinned && same(rows[index]!.snapshot,first.snapshot))group.push(rows[index++]!);
    if(group.every(row=>contents.has(row.message.id)))continue;
    if(first===view.pinned){const parent=(await sdk.Pinned({channel:view.channel,rootId:first.message.id,snapshot:first.snapshot},{signal})).pinnedRoot;if(!same(parent.snapshot,first.snapshot))throw new WireError('Cached history changed.','cache_reset',true);contents.put(parent.root);continue;}
    const highest=group.at(-1)!.message.creationSeq;
    let before=highest===first.snapshot.seq ? null : (BigInt(highest)+1n).toString();
    const expected=new Set(group.map(row=>row.message.id));
    for(let read=0;expected.size && read<group.length;read++){
      const page=view.root ? await sdk.Replies({channel:view.channel,rootId:view.root,before,snapshot:first.snapshot,limit:expected.size},{signal}).then(data=>data.replies)
        : await sdk.Roots({channel:view.channel,before,snapshot:first.snapshot,limit:expected.size},{signal}).then(data=>data.roots);
      signal.throwIfAborted();if(!same(page.snapshot,first.snapshot))throw new WireError('Cached history changed.','cache_reset',true);
      for(const message of page.items){if(!expected.delete(message.id))throw new WireError('Cached history changed.','cache_gap',true);contents.put(message);}
      if(!page.items.length || (expected.size && !page.nextBefore))throw new WireError('Cached history is incomplete.','cache_gap',true);
      if(page.nextBefore && before && compare(page.nextBefore,before)>=0)throw new WireError('Cached history made no progress.','cache_gap',true);
      before=page.nextBefore;
    }
    if(expected.size)throw new WireError('Cached history is incomplete.','cache_gap',true);
  }
}
