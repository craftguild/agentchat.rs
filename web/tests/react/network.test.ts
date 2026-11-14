import { test,expect,vi } from 'vitest';
import { createViewerClient,createContentCache } from '../../src/graphql/client.ts';
import { liveChanges } from '../../src/graphql/live.ts';
import { createSdk } from '../../src/graphql/sdk.ts';
import { envelope } from '../../src/graphql/network.ts';
import { LiveDocument } from '../../src/graphql/catalog.generated.ts';
import { MAX_FRAME_BYTES } from '../../src/sse.ts';
import { fixture,message,batch,cp } from './fixtures.ts';
const response=(data:unknown)=>new Response(JSON.stringify({data}));
test('finite reads retry twice; mutations retain their explicit retry boundary',async()=>{
  const fetch=vi.spyOn(globalThis,'fetch').mockResolvedValueOnce(new Response(JSON.stringify({errors:[{message:'busy',extensions:{code:'MOCK_ENVIRONMENT'}}]})))
    .mockResolvedValueOnce(new Response('',{status:500})).mockResolvedValueOnce(response({project:fixture.project}));
  const sdk=createSdk(createViewerClient()),signal=new AbortController().signal;
  expect((await sdk.Project(undefined,{signal})).project.id).toBe(fixture.project.id);expect(fetch).toHaveBeenCalledTimes(3);
  fetch.mockClear().mockImplementation(async()=>new Response(JSON.stringify({errors:[{message:'busy',extensions:{code:'MOCK_ENVIRONMENT'}}]})));
  await expect(sdk.Post({requestId:'request',channel:'dev',body:'body'},{signal})).rejects.toMatchObject({code:'MOCK_ENVIRONMENT'});expect(fetch).toHaveBeenCalledTimes(1);
});
test('finite abort settles its promise and cancels the owned response reader',async()=>{
  let cancelled=false;
  vi.spyOn(globalThis,'fetch').mockResolvedValue(new Response(new ReadableStream({cancel(){cancelled=true;}})));
  const abort=new AbortController(),pending=createSdk(createViewerClient()).Project(undefined,{signal:abort.signal});
  await new Promise(r=>setTimeout(r,5));abort.abort();
  await expect(Promise.race([pending,new Promise((_,reject)=>setTimeout(()=>reject(Error('stalled abort')),200))])).rejects.not.toThrow('stalled abort');
  expect(cancelled).toBe(true);
});
test('partial errors and all non-null delivery counters reject before publication',()=>{
  expect(envelope({data:{changes:fixture.changes},extensions:{chigiri:{delivery:{droppedTotal:null}}}}).data).toBeTruthy();
  for(const delivery of [{droppedTotal:0},{droppedTotal:8},{droppedTotal:'9007199254740993'},{}])
    expect(()=>envelope({data:{changes:fixture.changes},extensions:{chigiri:{delivery}}})).toThrow();
  expect(()=>envelope({data:{project:fixture.project},errors:[{message:'partial',extensions:{resetRequired:true}}]})).toThrow();
});
test('oversized JSON and invalid projections cannot enter Apollo cache',async()=>{
  const fetch=vi.spyOn(globalThis,'fetch').mockResolvedValue(response({project:{...fixture.project,checkpoint:{...fixture.project.checkpoint,seq:3}}}));
  const client=createViewerClient(),sdk=createSdk(client),signal=new AbortController().signal;
  await expect(sdk.Project(undefined,{signal})).rejects.toThrow();expect(Object.keys(client.cache.extract())).toHaveLength(0);
  fetch.mockResolvedValue(new Response('x'.repeat(MAX_FRAME_BYTES+1)));
  await expect(sdk.Project(undefined,{signal})).rejects.toMatchObject({reset:true,code:'response_overflow'});
});
test('SSE unsubscribe cancels the source and completes its physical teardown boundary',async()=>{
  let cancelled=false,closed=false;
  vi.spyOn(globalThis,'fetch').mockResolvedValue(new Response(new ReadableStream({cancel(){cancelled=true;}}),{headers:{'Content-Type':'text/event-stream'}}));
  const sub=createViewerClient().subscribe({query:LiveDocument,variables:{after:fixture.changes.after},context:{closed:()=>{closed=true;}}}).subscribe({next(){}});
  await new Promise(r=>setTimeout(r,5));sub.unsubscribe();
  await vi.waitFor(()=>{expect(cancelled).toBe(true);expect(closed).toBe(true);});
});
test('Apollo LRU bounds immutable content by bytes and clears normalized entries',()=>{
  const client=createViewerClient(),cache=createContentCache(client,1500);
  for(let i=1;i<8;i++)cache.put(message(i));
  expect(cache.has(message(1).id)).toBe(false);expect(cache.bytes).toBeLessThanOrEqual(1500);client.cache.gc();expect(cache.get(message(7).id)?.body).toBe('body 7');
  expect(Object.values(client.cache.extract()).filter((v:any)=>v?.__typename==='ViewerBody').length).toBeLessThan(7);
  cache.clear();expect(cache.bytes).toBe(0);expect(cache.has(message(7).id)).toBe(false);
});

test('a paged SSE replay cannot replace its fixed head with another valid frame range',async()=>{
  const one=batch(0,{...message(1),channelId:fixture.roots.items[0].channelId});one.head=cp(3);one.hasMore=true;
  const two=batch(2,{...message(3),channelId:fixture.roots.items[0].channelId},{...message(5),channelId:fixture.roots.items[0].channelId});
  const frames=[one,two].map(changes=>'event: next\ndata: '+JSON.stringify({data:{changes}})+'\n\n').join('');
  vi.spyOn(globalThis,'fetch').mockResolvedValue(new Response(frames,{headers:{'Content-Type':'text/event-stream'}}));
  const received:any[]=[];
  await new Promise<void>((resolve,reject)=>{
    createViewerClient().subscribe({query:LiveDocument,variables:{after:cp(0)},fetchPolicy:'no-cache'}).subscribe({next:value=>received.push(value),error:reject,complete:resolve});
  });
  expect(received.filter(v=>v.data)).toHaveLength(1);
  expect(received.find(v=>v.error)?.error).toMatchObject({code:'changed_head',reset:true});
});
test('the SDK preserves retry/reset classification through Apollo error-result completion',async()=>{
  vi.stubEnv('DEV',false);
  try{
    for(const [code,reset,retry] of [['MOCK_RESOURCE_LIMIT',false,true],['INVALID_CURSOR',true,false]] as const){
    const body='event: next\ndata: '+JSON.stringify({errors:[{message:'failure',extensions:{code,resetRequired:reset}}]})+'\n\n';
    vi.spyOn(globalThis,'fetch').mockResolvedValue(new Response(body,{headers:{'Content-Type':'text/event-stream'}}));
    const source=liveChanges(createSdk(createViewerClient()),cp(0),new AbortController().signal);
    await expect(source.next()).rejects.toMatchObject({code,retry,reset});
    await source.return(undefined);
    }
  }finally{vi.unstubAllEnvs();}
});
