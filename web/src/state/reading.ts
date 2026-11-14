import { compare, same, WireError } from '../types.ts';
import type { ChangeBatch, Checkpoint, Message } from '../types.ts';

export const MAX_TRACKING=20_000;
export type Post=Pick<Message,'id'|'channelId'|'rootId'|'creationSeq'|'createdAt'>;
export interface Section {received:Post|null;viewed:Post|null;boundary:Post|null;hideAt:number|null}
export interface Reading {
  scan:Checkpoint;pending:ReadonlyMap<string,Post>;ahead:ReadonlyMap<string,Post>;
  seen:ReadonlyMap<string,true>;received:ReadonlyMap<string,Post>;viewed:ReadonlyMap<string,Post>;
  sections:ReadonlyMap<string,Section>;
}
const sectionKey=(channel:string,root:string|null)=>`${channel}\0${root ?? ''}`;
const emptySection=():Section=>({received:null,viewed:null,boundary:null,hideAt:null});
const newer=(a:Post|null|undefined,b:Post)=>!a || compare(b.creationSeq,a.creationSeq)>0;
const copy=({id,channelId,rootId,creationSeq,createdAt}:Post):Post=>({id,channelId,rootId,creationSeq,createdAt});
function bounded(size:number):void {
  if(size>MAX_TRACKING)throw new WireError('Display history exceeded its buffer. Reload the snapshot.','unread_overflow',true);
}
/** All unread state belongs to this browser document, never server read receipts. */
export function startReading(scan:Checkpoint):Reading {
  return {scan,pending:new Map(),ahead:new Map(),seen:new Map(),received:new Map(),viewed:new Map(),sections:new Map()};
}
/** Preflight the entire frame and return a new state before committing progress. */
export function receiveReading(state:Reading,batch:ChangeBatch):Reading {
  if(batch.scan.projectId!==state.scan.projectId || compare(batch.after.seq,state.scan.seq)>0
    || (batch.scan.seq===state.scan.seq && !same(batch.scan,state.scan)))throw new WireError('Unread history changed.','unread_gap',true);
  if(compare(batch.scan.seq,state.scan.seq)<=0)return state;
  const pending=new Map(state.pending),ahead=new Map([...state.ahead].filter(([,p])=>compare(p.creationSeq,batch.scan.seq)>0));
  const received=new Map(state.received),sections=new Map(state.sections);
  for(const item of batch.items){
    if(item.kind!=='MESSAGE_CREATED' || compare(item.seq,state.scan.seq)<=0)continue;
    const post=copy(item.message);
    if(!state.ahead.has(post.id) && !pending.has(post.id))pending.set(post.id,post);
    if(newer(received.get(post.channelId),post))received.set(post.channelId,post);
    const key=sectionKey(post.channelId,post.rootId),old=sections.get(key) ?? emptySection();
    if(newer(old.received,post)){
      const unseen=newer(old.viewed,post),unfinished=old.received && newer(old.viewed,old.received);
      sections.set(key,{...old,received:post,boundary:unseen && !unfinished ? post : old.boundary,hideAt:unseen ? null : old.hideAt});
    }
  }
  bounded(pending.size+ahead.size);bounded(sections.size);
  return {...state,scan:batch.scan,pending,ahead,received,sections};
}
/** Only complete, actually visible articles may enter this transition. */
export function markReading(state:Reading,posts:readonly Post[],now:number):Reading {
  const pending=new Map(state.pending),ahead=new Map(state.ahead),seen=new Map(state.seen);
  const viewed=new Map(state.viewed),sections=new Map(state.sections);
  for(const value of posts){
    const post=copy(value);pending.delete(post.id);
    if(compare(post.creationSeq,state.scan.seq)>0 && !ahead.has(post.id))ahead.set(post.id,post);
    if(seen.has(post.id))continue;
    seen.set(post.id,true);if(seen.size>MAX_TRACKING)seen.delete(seen.keys().next().value!);
    if(newer(viewed.get(post.channelId),post))viewed.set(post.channelId,post);
    const key=sectionKey(post.channelId,post.rootId),old=sections.get(key) ?? emptySection();
    if(newer(old.viewed,post))sections.set(key,{...old,viewed:post,
      hideAt:old.boundary && old.received && !newer(post,old.received) && old.hideAt===null ? now+10_000 : old.hideAt});
  }
  bounded(pending.size+ahead.size);bounded(sections.size);
  return {...state,pending,ahead,seen,viewed,sections};
}
export function expireReading(state:Reading,now:number):Reading {
  let changed=false;const sections=new Map(state.sections);
  for(const [key,value] of sections)if(value.hideAt!==null && value.hideAt<=now){sections.set(key,{...value,boundary:null,hideAt:null});changed=true;}
  return changed ? {...state,sections} : state;
}
export function nextExpiry(state:Reading):number|null {
  let next:number|null=null;for(const value of state.sections.values())if(value.hideAt!==null && (next===null || value.hideAt<next))next=value.hideAt;return next;
}
export function unreadCounts(state:Reading):ReadonlyMap<string,number> {
  const counts=new Map<string,number>();for(const p of state.pending.values())counts.set(p.channelId,(counts.get(p.channelId)??0)+1);return counts;
}
export function unreadReplies(state:Reading,root:string):boolean {return [...state.pending.values()].some(p=>p.rootId===root);}
export function newestPost(state:Reading,channel:string):Post|null {
  const received=state.received.get(channel);return received && newer(state.viewed.get(channel),received) ? received : null;
}
export function sectionStart(state:Reading,channel:string,root:string|null):Post|null {
  return state.sections.get(sectionKey(channel,root))?.boundary ?? null;
}
