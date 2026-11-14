import type { ChangeBatch, Checkpoint, Message } from '../types.ts';
import { compare, same, WireError } from '../types.ts';

export const MAX_ROWS=2500;
export type Metadata=Omit<Message,'body'|'bodyHtml'|'renderFallback'>;
export interface Row {message:Metadata;snapshot:Checkpoint;countAt:Checkpoint}
export interface Anchor {id:string;offset:number}
export interface Timeline {
  channel:string;root:string|null;rows:Row[];pinned:Row|null;applied:Checkpoint;snapshot:Checkpoint;
  before:string|null;newerGap:boolean;busy:boolean;error:string;lastLoad:'newest'|'older';
  anchor:Anchor|null;target:{id:string;version:number}|null;revision:number;
}
export const viewKey=(channel:string,root:string|null)=>`${channel}\0${root ?? ''}`;
export function metadata(message:Message,snapshot:Checkpoint):Row {
  const {body:_body,bodyHtml:_html,renderFallback:_fallback,...record}=message;
  return {message:record,snapshot,countAt:snapshot};
}
export function emptyTimeline(channel:string,root:string|null,snapshot:Checkpoint):Timeline {
  return {channel,root,rows:[],pinned:null,applied:snapshot,snapshot,before:null,newerGap:false,busy:false,error:'',lastLoad:'newest',anchor:null,target:null,revision:0};
}
/** A reply updates counts only at a checkpoint at least as new as that projection. */
function updateCount(row:Row,message:Message,head:Checkpoint):Row {
  return compare(head.seq,row.countAt.seq)>=0 ? {...row,message:{...row.message,replyCount:message.replyCount},countAt:head} : row;
}
export function ingestTimeline(state:Timeline,batch:ChangeBatch):Timeline {
  if(state.applied.projectId!==batch.scan.projectId || compare(batch.after.seq,state.applied.seq)>0
    || (state.applied.seq===batch.scan.seq && !same(state.applied,batch.scan)))throw new WireError('History changed. Reload the snapshot.','view_gap',true);
  if(compare(batch.scan.seq,state.applied.seq)<=0)return state;
  const records=new Map(state.rows.map(row=>[row.message.id,row]));let pinned=state.pinned;
  for(const change of batch.items){
    if(change.kind!=='MESSAGE_CREATED' || compare(change.seq,state.applied.seq)<=0 || change.message.channel!==state.channel)continue;
    const message=change.message,root=records.get(message.threadId);
    if(root)records.set(root.message.id,updateCount(root,message,batch.head));
    if(pinned?.message.id===message.threadId)pinned=updateCount(pinned,message,batch.head);
    if((state.root===null ? message.rootId===null : message.rootId===state.root) && !records.has(message.id))records.set(message.id,metadata(message,batch.head));
  }
  const rows=[...records.values()].sort((a,b)=>compare(a.message.creationSeq,b.message.creationSeq));
  let before=state.before,snapshot=state.snapshot,newerGap=state.newerGap;
  if(rows.length>MAX_ROWS){
    const excess=rows.length-MAX_ROWS,anchor=rows.findIndex(r=>r.message.id===state.anchor?.id);
    if(anchor>=0 && anchor<excess){rows.splice(Math.min(anchor+64,rows.length-excess),excess);newerGap=true;}else rows.splice(0,excess);
    before=rows[0]!.message.creationSeq;snapshot=rows[0]!.snapshot;
  }
  return {...state,rows,pinned,applied:batch.scan,before,snapshot,newerGap,revision:state.revision+1};
}
export function prependTimeline(state:Timeline,older:Timeline):Timeline {
  const records=new Map(state.rows.map(r=>[r.message.id,r]));
  for(const row of older.rows){const current=records.get(row.message.id);if(!current || compare(current.countAt.seq,row.countAt.seq)<0)records.set(row.message.id,row);}
  const all=[...records.values()].sort((a,b)=>compare(a.message.creationSeq,b.message.creationSeq));
  return {...state,rows:all.slice(0,MAX_ROWS),before:older.before,snapshot:older.snapshot,newerGap:state.newerGap || all.length>MAX_ROWS,busy:false,error:'',revision:state.revision+1};
}
/** Replay must not be able to change the ID frozen by a navigation intent. */
export function targetTimeline(state:Timeline,id:string):Timeline {
  return {...state,anchor:null,target:{id,version:(state.target?.version ?? 0)+1}};
}
