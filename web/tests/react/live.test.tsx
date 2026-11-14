// @vitest-environment jsdom
import { StrictMode } from 'react';
import { renderHook,act,cleanup } from '@testing-library/react';
import { Observable } from 'rxjs';
import { test,expect,vi,afterEach,beforeEach } from 'vitest';
import { useLive } from '../../src/state/use-live.ts';
import type { ViewerSdk } from '../../src/graphql/sdk.ts';
import { WireError } from '../../src/types.ts';
import { batch,cp,message,fixture } from './fixtures.ts';
beforeEach(()=>vi.stubEnv('DEV',false));
afterEach(()=>{cleanup();vi.useRealTimers();vi.unstubAllEnvs();});
const aborted=(signal:AbortSignal)=>new Promise<void>(resolve=>{if(signal.aborted)resolve();else signal.addEventListener('abort',()=>resolve(),{once:true});});
const flush=async()=>{await act(async()=>{for(let i=0;i<40;i++)await Promise.resolve();});};
function sourceSdk(source:(after:ReturnType<typeof cp>,signal:AbortSignal)=>AsyncGenerator):ViewerSdk {
  return {Live:({after},options)=>new Observable(observer=>{
    const own=new AbortController(),abort=()=>own.abort();options.signal.addEventListener('abort',abort,{once:true});
    const running=(async()=>{
      options.connected();
      for await(const frame of source(after,own.signal)){
        const changes={...frame,items:frame.items.map(item=>item.message?{...item,message:{...item.message,channelId:fixture.roots.items[0].channelId}}:item)};
        observer.next({changes});for(let i=0;i<4;i++)await Promise.resolve();
      }
      await aborted(own.signal);
    })().catch(error=>observer.error(error)).finally(()=>{options.signal.removeEventListener('abort',abort);options.closed();});
    return()=>{own.abort();void running;};
  })} as unknown as ViewerSdk;
}
test('StrictMode, hide and resume never overlap sources; duplicates do not reapply',async()=>{
  Object.defineProperty(document,'visibilityState',{value:'visible',configurable:true});
  const value=batch(0,message(1)),starts:string[]=[];let active=0,max=0,at=cp(0),applied=0;
  const sdk=sourceSdk(async function*(after,signal){starts.push(after.seq);active++;max=Math.max(max,active);try{if(after.seq==='0'){yield value;yield value;}await aborted(signal);}finally{await Promise.resolve();active--;}});
  const result=renderHook(()=>useLive({enabled:true,sdk,progress:()=>at,apply:b=>{at=b.scan;applied++;},status:()=>{}}),{wrapper:StrictMode});
  await flush();expect(applied).toBe(1);expect(active).toBe(1);
  await act(async()=>{Object.defineProperty(document,'visibilityState',{value:'hidden',configurable:true});document.dispatchEvent(new Event('visibilitychange'));});await flush();expect(active).toBe(0);
  await act(async()=>{Object.defineProperty(document,'visibilityState',{value:'visible',configurable:true});document.dispatchEvent(new Event('visibilitychange'));});await flush();expect(starts).toEqual(['0',value.scan.seq]);expect(max).toBe(1);
  result.unmount();await flush();expect(active).toBe(0);
});
test('application overflow and cursor gaps retain progress and stop in reset',async()=>{
  for(const overflow of [true,false]){
    const at=cp(0),status=vi.fn(),apply=vi.fn(()=>{throw new WireError('Full','overflow',true);});
    const value=batch(overflow?0:1,message(2)),sdk=sourceSdk(async function*(){yield value;});
    const result=renderHook(()=>useLive({enabled:true,sdk,progress:()=>at,apply,status}));await flush();
    expect(status).toHaveBeenLastCalledWith('reset',expect.any(String));expect(at.seq).toBe('0');expect(apply).toHaveBeenCalledTimes(overflow?1:0);result.unmount();
  }
});
test('one loop uses 1/2/5/10 second backoff, resets only after progress and cancels on unmount',async()=>{
  vi.useFakeTimers();let starts=0,at=cp(0),applied=0;
  const sdk=sourceSdk(async function*(){starts++;if(starts===3)yield batch(0,message(1));throw new WireError('Temporary','runtime_unavailable',false,true);});
  const result=renderHook(()=>useLive({enabled:true,sdk,progress:()=>at,apply:b=>{at=b.scan;applied++;},status:()=>{}}));await flush();expect(starts).toBe(1);
  for(const delay of [1000,2000,1000,2000,5000,10000,10000]){
    const before=starts;await act(()=>vi.advanceTimersByTimeAsync(delay-1));expect(starts).toBe(before);await act(()=>vi.advanceTimersByTimeAsync(1));expect(starts).toBe(before+1);
  }
  expect(applied).toBe(1);result.unmount();await act(()=>vi.advanceTimersByTimeAsync(100000));expect(starts).toBe(8);
});
