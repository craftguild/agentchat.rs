import { test,expect,vi } from 'vitest';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { firstValueFrom } from 'rxjs';
import { getSdk } from '../../src/graphql/catalog.generated.ts';
import { createApolloRequester } from '../../src/graphql/requester.ts';
import { createViewerClient } from '../../src/graphql/client.ts';
import { fixture } from './fixtures.ts';

test('adding a catalog query generates an executable typed SDK method without a handwritten method',async()=>{
  // Real generator startup includes cold Node module loading, with its own process budget.
  const {stdout}=await promisify(execFile)(process.execPath,['tests/react/generate-sdk-probe.mjs'],{cwd:process.cwd(),timeout:25_000});
  expect(stdout).toContain('PASS: new catalog operation generates an executable SDK method');
},30_000);
test('generated query/mutation/subscription methods share Apollo request execution and envelopes',async()=>{
  const fetch=vi.spyOn(globalThis,'fetch').mockImplementation(async(_url,options)=>{
    const request=JSON.parse(String(options!.body));
    if(request.operationName==='Live')return new Response('event: next\ndata: '+JSON.stringify({data:{changes:fixture.changes}})+'\n\n',{headers:{'Content-Type':'text/event-stream'}});
    return new Response(JSON.stringify({data:request.operationName==='Project'?{project:fixture.project}:{result:{id:'posted',warnings:[]}}}));
  });
  const sdk=getSdk(createApolloRequester(createViewerClient())),signal=new AbortController().signal;
  expect((await sdk.Project(undefined,{signal})).project.id).toBe(fixture.project.id);
  expect((await sdk.Post({requestId:'stable-id',channel:'dev',body:'raw body'},{signal})).result.id).toBe('posted');
  expect((await firstValueFrom(sdk.Live({after:fixture.changes.after},{signal}))).changes.scan).toEqual(fixture.changes.scan);
  const requests=fetch.mock.calls.map(([,options])=>JSON.parse(String(options!.body)));
  expect(requests.map(r=>r.operationName)).toEqual(['Project','Post','Live']);
  expect(requests[1].variables).toEqual({requestId:'stable-id',channel:'dev',body:'raw body'});
});
test('Apollo result checkpoints can be reused as inputs without typename fields or body changes',async()=>{
  const checkpoint={...fixture.project.checkpoint,__typename:'Checkpoint'};
  const fetch=vi.spyOn(globalThis,'fetch').mockImplementation(async(_url,options)=>{
    const request=JSON.parse(String(options!.body));
    return new Response(JSON.stringify({data:request.operationName==='Project'?{project:{...fixture.project,checkpoint}}:
      request.operationName==='Roots'?{roots:fixture.roots}:{result:{id:'posted',warnings:[]}}}));
  });
  const sdk=getSdk(createApolloRequester(createViewerClient()));
  const {project}=await sdk.Project();expect(project.checkpoint).toHaveProperty('__typename','Checkpoint');
  await sdk.Roots({channel:'dev',before:null,snapshot:project.checkpoint,limit:1});
  const body='{"__typename":"ordinary message text"}';await sdk.Post({requestId:'same-id',channel:'dev',body});
  const requests=fetch.mock.calls.map(([,options])=>JSON.parse(String(options!.body)));
  expect(requests[1].variables.snapshot).toEqual(fixture.project.checkpoint);
  expect(requests[2].variables.body).toBe(body);
  expect(project.checkpoint).toHaveProperty('__typename','Checkpoint');
});
