import type { OperationDefinitionNode } from 'graphql';
import type { Sdk } from './catalog.generated.ts';
import * as decode from '../validate.ts';
import { same,WireError } from '../types.ts';
import { checkMessageReceipts } from './receipt-validation.ts';

type Checker=(data:Record<string,unknown>,variables:Record<string,unknown>)=>unknown;
const posted=(value:unknown)=>{
  const row=decode.record(value);decode.text(row.id);
  if(!Array.isArray(row.warnings))throw new WireError('Invalid post response.','invalid_frame',true);
  for(const warning of row.warnings){const item=decode.record(warning);decode.text(item.code);decode.text(item.handle);}
};
/** App-specific wire and snapshot invariants; operation signatures are generated. */
const checks={
  Project:data=>decode.project(data.project),
  Channels:data=>decode.channels(data.channels),
  Agents:data=>decode.agents(data.agents),
  Members:data=>decode.actors(data.channelMembers),
  MessageReceipts:checkMessageReceipts,
  Roots:data=>decode.window(data.roots),
  Replies:data=>decode.window(data.replies),
  Pinned:data=>decode.pinned(data.pinnedRoot),
  Changes:(data,variables)=>{
    const batch=decode.batch(data.changes);
    if(!same(batch.after,decode.checkpoint(variables.after))||!same(batch.head,decode.checkpoint(variables.through)))
      throw new WireError('Replay range changed.','changed_range',true);
  },
  Validate:(data,variables)=>{
    const head=decode.checkpoint(decode.record(data.changes).head);
    if(!same(head,decode.checkpoint(variables.through)))throw new WireError('Snapshot changed.','snapshot_changed',true);
  },
  Create:data=>{const row=decode.record(data.createChannel);decode.text(row.id);decode.text(row.name);},
  Post:data=>posted(data.result),
  Reply:data=>posted(data.result),
  Live:data=>decode.batch(data.changes),
} satisfies Record<keyof Sdk,Checker>;

/** Never publish unvalidated data, including newly added catalog operations. */
export function checkResult(operation:OperationDefinitionNode,variables:Record<string,unknown>,data:unknown):void {
  const name=operation.name?.value;
  if(!name||!Object.hasOwn(checks,name))throw new WireError('Unknown viewer operation.','invalid_operation',true);
  const check:Checker=checks[name as keyof typeof checks];check(decode.record(data),variables);
}
