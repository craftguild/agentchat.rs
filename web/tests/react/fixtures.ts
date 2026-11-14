import fixtureJson from '../../fixtures/contract-v1.json';
import type { Message, Checkpoint, ChangeBatch } from '../../src/types.ts';
export const fixture=fixtureJson;
export const id=(n:number)=>`00000000-0000-0000-0000-${String(n).padStart(12,'0')}`;
export const cp=(seq:string|number):Checkpoint=>({...fixture.project.checkpoint,seq:String(seq),eventId:String(seq)==='0'?null:id(Number(BigInt(seq)%1000000n)+10000)});
export const message=(seq:number,rootId:string|null=null,channelId='room'):Message=>({...fixture.roots.items[0],id:id(seq),channelId,channel:'dev',rootId,threadId:rootId??id(seq),creationSeq:String(seq),body:`body ${seq}`,bodyHtml:`<p>body ${seq}</p>`,replyCount:'0'});
export function batch(after:number,...messages:Message[]):ChangeBatch {
  const head=cp(Math.max(after,...messages.map(m=>Number(m.creationSeq)+1)));
  return {after:cp(after),head,scan:head,hasMore:false,items:messages.map(m=>({kind:'MESSAGE_CREATED',eventId:id(Number(m.creationSeq)+10001),seq:String(Number(m.creationSeq)+1),channel:null,message:m}))};
}
