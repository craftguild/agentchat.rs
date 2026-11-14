import { useEffect, useRef } from 'react';
import type { ViewerSdk } from '../graphql/sdk.ts';
import {liveChanges} from '../graphql/live.ts';
import { WireError, compare, same } from '../types.ts';
import type { Checkpoint, ChangeBatch } from '../types.ts';

export type ConnectionState='live'|'connecting'|'hidden'|'reset'|'error';
interface Options {
  enabled:boolean;sdk:ViewerSdk;progress:()=>Checkpoint;apply:(batch:ChangeBatch)=>void;
  status:(state:ConnectionState,message?:string)=>void;
}
function delay(ms:number,signal:AbortSignal):Promise<void> {
  return new Promise((resolve,reject)=>{
    signal.throwIfAborted();const timer=setTimeout(()=>{signal.removeEventListener('abort',abort);resolve();},ms);
    const abort=()=>{clearTimeout(timer);reject(signal.reason);};signal.addEventListener('abort',abort,{once:true});
  });
}
/** Serialize teardown/start, including StrictMode, before opening another source. */
export function useLive(options:Options):void {
  const latest=useRef(options);latest.current=options;
  const tail=useRef<Promise<void>>(Promise.resolve());
  useEffect(()=>{
    if(!options.enabled)return;
    let closed=false,controller:AbortController|null=null;
    const refresh=()=>{
      controller?.abort();const own=new AbortController();controller=own;
      const previous=tail.current;
      tail.current=(async()=>{
        await previous;if(closed || own.signal.aborted)return;
        if(document.visibilityState==='hidden'){latest.current.status('hidden');return;}
        let attempt=0;
        while(!own.signal.aborted && !closed){
          latest.current.status('connecting');
          try {
            for await(const batch of liveChanges(latest.current.sdk,latest.current.progress(),own.signal,()=>latest.current.status('live'))){
              own.signal.throwIfAborted();const at=latest.current.progress();
              if(batch.scan.projectId!==at.projectId || compare(batch.after.seq,at.seq)>0
                || (batch.scan.seq===at.seq && !same(batch.scan,at)))throw new WireError('History changed. Reload the snapshot.','cursor_gap',true);
              if(compare(batch.scan.seq,at.seq)<=0)continue;
              latest.current.apply({...batch,items:batch.items.filter(item=>compare(item.seq,at.seq)>0)});
              attempt=0;latest.current.status('live');
            }
            throw new WireError('Connection completed.','source_complete',false,true);
          }catch(error){
            if(closed || own.signal.aborted)return;
            if(error instanceof WireError && (error.reset || !error.retry)){latest.current.status(error.reset?'reset':'error',error.message);return;}
            latest.current.status('connecting','Connection interrupted. Reconnecting…');
            try{await delay([1000,2000,5000,10000][Math.min(attempt++,3)]!,own.signal);}catch{return;}
          }
        }
      })();
    };
    document.addEventListener('visibilitychange',refresh);refresh();
    return()=>{closed=true;controller?.abort();document.removeEventListener('visibilitychange',refresh);};
  },[options.enabled,options.sdk]);
}
