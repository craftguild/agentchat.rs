import type { ViewerSdk } from './sdk.ts';
import * as decode from '../validate.ts';
import { same,compare,WireError } from '../types.ts';
import type { ChangeBatch,Checkpoint } from '../types.ts';

/** Adapt generated Live Observable into one bounded, physically owned pull source. */
export async function* liveChanges(sdk:ViewerSdk,after:Checkpoint,signal:AbortSignal,connected?:()=>void):AsyncGenerator<ChangeBatch> {
  const queue:{batch:ChangeBatch;bytes:number}[]=[];let queued=0,error:unknown,wake:()=>void=()=>{};
  let ready!:()=>void,reject!:(e:unknown)=>void,closed!:()=>void;
  const finished=new Promise<void>(resolve=>{closed=resolve;});
  const setup=new Promise<void>((resolve,fail)=>{ready=resolve;reject=fail;});
  const abort=()=>{error=signal.reason??new DOMException('Aborted','AbortError');reject(error);wake();};
  const subscription=sdk.Live({after},{signal,connected:()=>{ready();connected?.();},closed}).subscribe({
    next:result=>{
      try{
        const batch=decode.batch(result.changes),bytes=JSON.stringify(result).length*2;
        if(queued+bytes>4*1024*1024||queue.length>=64)throw new WireError('Live data exceeded its buffer.','view_overflow',true);
        queue.push({batch,bytes});queued+=bytes;wake();
      }catch(e){error=e;reject(e);wake();}
    },error:e=>{error=e;reject(e);wake();},
    complete:()=>{error??=new WireError('Live connection completed. Reconnecting…','source_complete',false,true);reject(error);wake();},
  });
  signal.addEventListener('abort',abort,{once:true});if(signal.aborted)abort();
  try{
    await setup;
    if(import.meta.env?.DEV){
      // Mock has no replay: register Live before taking and draining a fixed head.
      const head=(await sdk.Project(undefined,{signal})).project.checkpoint;let cursor=after;
      if(head.projectId!==after.projectId||compare(head.seq,after.seq)<0||(head.seq===after.seq&&!same(head,after)))throw new WireError('Development history changed.','changed_range',true);
      while(compare(cursor.seq,head.seq)<0){
        const batch=decode.batch((await sdk.Changes({after:cursor,through:head},{signal})).changes);
        if(compare(batch.scan.seq,cursor.seq)<=0)throw new WireError('Replay made no progress.','cursor_gap',true);
        yield batch;cursor=batch.scan;
      }
    }
    for(;;){
      if(error)throw error;
      const next=queue.shift();if(next){queued-=next.bytes;yield next.batch;continue;}
      await new Promise<void>(resolve=>{wake=resolve;if(error||queue.length)resolve();});
    }
  }finally{signal.removeEventListener('abort',abort);subscription.unsubscribe();await finished;}
}
