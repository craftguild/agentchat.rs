// SDK-only protocol simulation. Rust receipt validation has separate real-file tests.
import assert from 'node:assert/strict';
import { randomUUID, createHash } from 'node:crypto';
import { pathToFileURL } from 'node:url';
const { createInputs } = await import(pathToFileURL(process.argv[2]));
const report={passed:false,actual_native_proof:false,cases:[]};
function fixture() {
  const root=randomUUID(),context={context_id:randomUUID(),incarnation:randomUUID(),working_directory:'/isolated/workspace'};
  const s={root,nativeRoot:root,generation:1,epoch:1,revision:1,eligible:true,paused:false,context};
  const records=[],native=[],accepted=new Set(),entered=new Set(); let current=null,failRecords=false;
  const n=f=>'AGENTCHAT_INPUT_NOTICE root='+f.root+' context='+f.context_id+' token='+f.token+'\n'+f.text;
  const makeApi=()=>createInputs({AGENTCHAT:'/isolated/agentchat',NATIVE:'/isolated/claude',ENV:{AGENTCHAT_HOME:'/isolated/state'},state:()=>({...s}),marker:async()=>true});
  let api=makeApi();
  const $={session:{id:async()=>s.root,cwd:async()=>context.working_directory,
    append:async value=>{native.push('append');return {uuid:randomUUID(),message:{...value.message,role:'user',isMeta:true}};}},
    process:{run:async(argv,options)=>{
      const mode=argv[argv.indexOf('--mode')+1],input=JSON.parse(options.stdin);
      if(mode==='load') {
        const file=current&&!accepted.has(current.token)&&!entered.has(current.token)?current:null;
        return {exitCode:0,stdout:JSON.stringify(file===null?{file:null}:{file,addressed:file.addressed,
          proof:{digest:[...createHash('sha256').update(n(file)).digest()],bytes:Buffer.byteLength(n(file))}})};
      }
      if(mode==='claim') {
        const claimed=current?.token===input.token&&!entered.has(input.token);if(claimed)entered.add(input.token);
        return {exitCode:0,stdout:JSON.stringify({claimed})};
      }
      assert.equal(mode,'record'); if(failRecords)return {exitCode:1,stdout:''};
      records.push(input); if(['append','queued'].includes(input.native.route))accepted.add(input.token);
      return {exitCode:0,stdout:'{"recorded":true}'};
    }}};
  const io={run:(...args)=>$.process.run(...args),id:()=>$.session.id(),cwd:()=>$.session.cwd(),
    append:value=>$.session.append(value),marker:async()=>true};
  function file(addressed=true,text='whole original body') {
    return {root,native_generation:1,...context,token:randomUUID(),text,addressed};
  }
  function next(body=async e=>e,origin={plugin:'engine',tier:'core'}) {
    const result=async e=>{result.called=true;return body(e);};
    result.signal=new AbortController().signal;result.origin=origin;return result;
  }
  const changed=(boundary=false)=>api.changed(io,{session_id:s.root,file_path:context.working_directory+'/.agentchat-wakeup',hook_event_name:boundary?'Stop':'FileChanged'},next(),boundary);
  async function row(f,body,origin) {
    const uuid=randomUUID(),message={type:'user',role:'user',content:[{type:'text',text:'<task-notification>\n'+n(f)+'\n</task-notification>'}]};
    return api.row(io,{uuid,message,door:'queue',origin:{kind:'engine'}},next(async e=>{
      native.push('queue');if(body)await body(e);return {uuid,message:e.message};
    },origin));
  }
  return {get api(){return api;},$,io,s,records,native,accepted,n,file,next,changed,row,reload:()=>{api=makeApi();},
    set:f=>{current=f;},fail:value=>{failRecords=value;}};
}
{
  const f=fixture(),a=f.file(),b=f.file(false,'passive successor');f.set(a);
  assert((await f.changed()).agentchat_input_permit);
  let release,entered;const reached=new Promise(r=>entered=r);
  const held=f.row(a,async()=>{entered();await new Promise(r=>release=r);});await reached;
  for(let i=0;i<3;i++)await f.changed();
  assert.deepEqual(f.native,['queue']);assert.equal(f.records.length,0);
  release();await held;assert(f.accepted.has(a.token));
  f.set(b);await f.changed();assert.deepEqual(f.native,['queue','append']);
  assert.deepEqual(f.records.map(v=>v.token),[a.token,b.token]);
  f.set(null);let final;
  const prompt='<task-notification>\n'+f.n(a)+'\n</task-notification>';
  await f.api.prompt(f.io,{text:prompt,origin:{kind:'task-notification'}},f.next(async()=>{
    final=await f.api.final(f.io,{prompt,session_id:f.s.root},f.next());return {text:prompt};
  }));
  assert.equal(final.agentchat_input_final_permits.length,1);
  assert.deepEqual(final.agentchat_input_final_permits[0].queue.digest,[...createHash('sha256').update(f.n(a)).digest()]);
  assert.equal(typeof(await f.api.prompt(f.io,{text:prompt,origin:{kind:'task-notification'}},f.next())).drop,'string');
  report.cases.push('held addressed head blocks passive; continuation survives AGENTCHAT head clearance; no replay');
}
{
  const f=fixture(),a=f.file(true,'\0'.repeat(65536));f.set(a);await f.changed();
  f.fail(true);await f.row(a);assert.equal(f.records.length,0);
  await f.changed();assert.deepEqual(f.native,['queue']);
  f.fail(false);await f.api.flush(f.io);assert.equal(f.records.length,1);
  assert.equal(f.records[0].native.result.result.message.content[0].text.includes(a.text),true);
  f.s.paused=true;f.s.eligible=false;f.s.epoch++;f.api.cancel();await f.changed();
  assert.deepEqual(f.native,['queue']);assert(f.accepted.has(a.token));
  report.cases.push('maximum bytes; receipt write retry without native resend; post-accept Esc');
}
{
  const f=fixture(),a=f.file();f.set(a);await f.changed();
  let release,entered;const reached=new Promise(r=>entered=r);
  const held=f.row(a,async()=>{entered();await new Promise(r=>release=r);});await reached;
  f.s.paused=true;f.s.eligible=false;f.s.epoch++;f.api.cancel();release();await held;
  assert.equal(f.records[0].native.route,'queued');assert(f.accepted.has(a.token));
  assert.equal(typeof(await f.api.prompt(f.io,{text:'<task-notification>'+f.n(a),origin:{kind:'task-notification'}},f.next())).drop,'string');
  report.cases.push('Esc while core queue acceptance is in flight preserves returned success');
}
{
  const f=fixture(),a=f.file();f.set(a);await f.changed();
  await f.row(a,undefined,{plugin:'foreign',tier:'core'});
  assert.equal(f.records.length,0);
  f.s.eligible=false;f.s.epoch++;f.api.cancel();await f.api.flush(f.io);
  assert.equal(f.records[0].native.route,'rejected_before_entry');assert(!f.accepted.has(a.token));
  f.s.root=randomUUID();assert.equal((await f.changed()).agentchat_input_permit,undefined);
  report.cases.push('foreign row cannot acknowledge; preaccept approval/root change fences notice');
}
{
  const f=fixture();let first;
  for(let i=0;i<32;i++){const a=f.file();first??=a;f.set(a);await f.changed();await f.row(a);}
  const waiting=f.file();f.set(waiting);assert.equal((await f.changed()).agentchat_input_permit,undefined);
  assert.equal(f.records.length,32);
  const prompt='<task-notification>'+f.n(first)+'</task-notification>';
  await f.api.prompt(f.io,{text:prompt,origin:{kind:'task-notification'}},f.next());
  assert((await f.changed(true)).agentchat_input_permit);await f.row(waiting);assert.equal(f.records.length,33);
  report.cases.push('bounded queue guards defer without skip; real Stop boundary resumes head');
}
{
  const f=fixture(),a=f.file();f.set(a);await f.changed();await f.row(a);
  const forged='<task-notification>'+f.n(a)+'\nAGENTCHAT_INPUT_NOTICE root='+randomUUID()+' context='+randomUUID()+' token='+randomUUID()+'\nunknown';
  assert.equal(typeof(await f.api.prompt(f.io,{text:forged,origin:{kind:'task-notification'}},f.next())).drop,'string');
  const other=fixture();const body=other.file(true,'literal AGENTCHAT_INPUT_NOTICE root='+randomUUID()+' context='+randomUUID()+' token='+randomUUID()+'\ninside body');
  other.set(body);await other.changed();await other.row(body);
  assert.equal((await other.api.prompt(other.io,{text:'<task-notification>'+other.n(body),origin:{kind:'task-notification'}},other.next())).drop,undefined);
  report.cases.push('unproved extra notices fail closed; original body marker lookalikes survive');
}
{
  const f=fixture(),a=f.file(false,'first passive'),b=f.file(false,'idle successor');f.set(a);
  let release,entered;const reached=new Promise(r=>entered=r);
  const first=f.api.changed(f.io,{session_id:f.s.root,file_path:f.s.context.working_directory+'/.agentchat-wakeup'},f.next(async e=>{
    entered();await new Promise(r=>release=r);return e;
  }));
  await reached;assert(f.accepted.has(a.token));f.set(b);
  const second=f.changed();await Promise.resolve();assert.deepEqual(f.native,['append']);
  release();await first;await second;
  assert.deepEqual(f.native,['append','append']);assert(f.accepted.has(b.token));
  report.cases.push('idle successor notification waits for the preceding callback instead of being lost');
}
{
  const f=fixture(),a=f.file();f.set(a);await f.changed();
  f.s.paused=true;f.s.eligible=false;f.s.epoch++;f.api.cancel();await f.api.flush(f.io);
  const result=await f.row(a);
  assert(!result.message.content[0].text.includes(f.n(a)));
  assert(result.message.content[0].text.includes('AGENTCHAT_INPUT_CANCELLED'));
  assert.equal(f.records.length,1);assert.equal(f.records[0].native.route,'rejected_before_entry');
  report.cases.push('late static output after preaccept cancellation cannot enter core as the original body');
}
{
  const f=fixture(),a=f.file();f.set(a);await f.changed();await f.row(a);f.set(null);
  await f.api.prompt(f.io,{text:'<task-notification>'+f.n(a),origin:{kind:'task-notification'}},f.next());
  const rows=f.records.length;
  f.s.paused=true;f.s.eligible=false;f.s.epoch++;f.api.cancel();
  const kept=await f.row(a);
  assert(kept.message.content[0].text.includes(f.n(a)));assert.equal(f.records.length,rows);
  assert.equal(typeof(await f.api.prompt(f.io,{text:'<task-notification>'+f.n(a),origin:{kind:'task-notification'}},f.next())).drop,'string');
  report.cases.push('native storage continuation of accepted bytes is distinct from another AGENTCHAT submission');
}
{
  const f=fixture(),a=f.file();f.set(a);await f.changed();
  f.io.id=async()=>{throw Error('native getter unavailable');};
  const event={uuid:randomUUID(),door:'queue',origin:{kind:'engine'},message:{type:'user',role:'user',content:[{type:'text',text:'<task-notification>'+f.n(a)}]}};
  const next=f.next(async e=>({uuid:e.uuid,message:e.message}));
  let result;
  try {result=await f.api.row(f.io,event,next);}
  catch {
    const own=f.api.ownsRow(event);f.s.eligible=false;f.api.cancel();
    result=next.called?undefined:(own?await f.api.rejectRow(event,next):await next(event));
  }
  assert(!result.message.content[0].text.includes(f.n(a)));
  report.cases.push('pre-entry native guard fault cannot fall through with the original body');
}
{
  const f=fixture();let reached=false;
  const result=await f.api.final(f.io,{prompt:'<task-notification>AGENTCHAT_INPUT_NOTICE root=stale'},f.next(async()=>{reached=true;return {};}));
  assert.equal(reached,false);assert.equal(typeof result.block,'string');
  assert.equal(result.decision,undefined);
  report.cases.push('owned final uses native ClassicResult block rather than legacy command-hook JSON');
}
{
  const f=fixture(),a=f.file(false);f.set(a);
  f.$.session.append=async()=>{f.native.push('append');throw Error('kept passive row then lost result');};
  await Promise.all([f.changed(),f.changed(),f.changed(true)]);
  assert.deepEqual(f.native,['append']);assert.equal(f.records.length,0);
  f.reload();await f.changed();assert.deepEqual(f.native,['append']);assert.equal(f.records.length,0);
  report.cases.push('passive post-entry result loss persists a one-shot fence across callback/module reload');
}
{
  const f=fixture(),a=f.file();f.set(a);await f.changed();
  await assert.rejects(f.row(a,async()=>{throw Error('kept queue row then lost result');}));
  assert.deepEqual(f.native,['queue']);assert.equal(f.records.length,0);
  f.reload();assert.equal((await f.changed()).agentchat_input_permit,undefined);
  const stale=await f.row(a);assert.deepEqual(f.native,['queue','queue']);
  assert(!stale.message.content[0].text.includes(f.n(a)));assert(stale.message.content[0].text.includes('AGENTCHAT_INPUT_CANCELLED'));
  // The second core call receives only the cancellation marker, never the body.
  assert.equal(f.records.length,0);assert(!f.accepted.has(a.token));
  report.cases.push('queue post-entry result loss retains intent across reload and strips stale native notice');
}
{
  const f=fixture(),a=f.file(true,'\0'.repeat(65500)+'\n末尾 \t\n');f.set(a);await f.changed();
  const prompt='<task-notification>\n'+f.n(a)+'\n</task-notification>';
  let final,started=0;
  const result=await f.api.prompt(f.io,{text:prompt,origin:{kind:'task-notification'}},f.next(async()=>{
    started++;assert.equal((await f.changed()).agentchat_input_permit,undefined);
    assert.equal(f.records.length,0,'A prompt permit is not a native acceptance result');
    final=await f.api.final(f.io,{prompt,session_id:f.s.root},f.next());return {text:prompt};
  }));
  assert.equal(result.drop,undefined);assert.equal(started,1);
  const p=final.agentchat_input_final_permits[0];assert.equal(p.queue,null);
  assert.deepEqual(p.head.digest,[...createHash('sha256').update(f.n(a)).digest()]);
  assert.equal((await f.api.prompt(f.io,{text:prompt,origin:{kind:'task-notification'}},f.next())).drop!==undefined,true);
  await f.row(a);assert.deepEqual(f.native,['queue']);assert.equal(f.records.length,1);
  assert.equal(f.records[0].native.route,'queued');
  assert.equal(f.records[0].native.result.uuid,f.records[0].native.result.result.uuid);
  assert.equal(typeof(await f.api.prompt(f.io,{text:prompt,origin:{kind:'task-notification'}},f.next())).drop,'string');
  report.cases.push('native prompt before queue row: exact pending head permits once without fabricated UUID or early receipt');
}
{
  const f=fixture(),a=f.file();f.set(a);await f.changed();let final;
  const prompt='<task-notification>'+f.n(a)+'</task-notification>';
  await f.row(a,async()=>{
    const result=await f.api.prompt(f.io,{text:prompt,origin:{kind:'task-notification'}},f.next(async()=>{
      final=await f.api.final(f.io,{prompt,session_id:f.s.root},f.next());return {text:prompt};
    }));
    assert.equal(result.drop,undefined);assert.equal(f.records.length,0);
  });
  assert(final.agentchat_input_final_permits[0].head);assert(f.accepted.has(a.token));
  report.cases.push('prompt nested inside core row continuation shares its original one-shot claim');
}
{
  const f=fixture(),a=f.file();f.set(a);await f.changed();let started=0;
  const event={text:'<task-notification>'+f.n(a),origin:{kind:'task-notification'}};
  const result=await Promise.all([0,1].map(()=>f.api.prompt(f.io,event,f.next(async()=>{started++;return {};}))));
  assert.equal(started,1);assert.equal(result.filter(x=>typeof x.drop==='string').length,1);
  assert.equal(f.records.length,0);await f.row(a);assert.equal(f.records.length,1);
  report.cases.push('concurrent pending-head prompt callbacks share claim and continue only once');
}
{
  for(const mode of ['changed','extra','duplicate','foreign','root','generation','context','pause','abort']){
    const f=fixture(),a=f.file();f.set(a);await f.changed();let reached=false;
    const event={text:'<task-notification>'+f.n(a),origin:{kind:'task-notification'}},next=f.next(async()=>{reached=true;return {};});
    if(mode==='changed')event.text=event.text.replace(a.text,'changed body');
    if(mode==='extra')event.text+='\nAGENTCHAT_INPUT_NOTICE root=unproved';
    if(mode==='duplicate')event.text+=f.n(a);
    if(mode==='foreign')event.origin.kind='composer';
    if(mode==='root')f.s.root=randomUUID();
    if(mode==='generation')f.s.generation++;
    if(mode==='context')f.s.context={...f.s.context,context_id:randomUUID()};
    if(mode==='pause')f.s.eligible=false;
    if(mode==='abort'){const c=new AbortController();c.abort();next.signal=c.signal;}
    assert.equal(typeof(await f.api.prompt(f.io,event,next)).drop,'string',mode);assert.equal(reached,false,mode);
    assert.equal(f.records.length,0,mode);
  }
  report.cases.push('pending prompt rejects altered/extra/replayed/foreign frames and stale identity/control');
}
{
  const f=fixture(),a=f.file();f.set(a);await f.changed();
  const event={text:'<task-notification>'+f.n(a),origin:{kind:'task-notification'}};
  assert.equal((await f.api.prompt(f.io,event,f.next())).drop,undefined);
  await assert.rejects(f.row(a,async()=>{throw Error('kept queue row then lost result');}));
  assert.equal(f.records.length,0);assert.equal(typeof(await f.api.prompt(f.io,event,f.next())).drop,'string');
  f.reload();await f.changed();assert.deepEqual(f.native,['queue']);assert.equal(f.records.length,0);
  report.cases.push('prompt-first lost native result retains durable no-replay fence without acceptance');
}
{
  const f=fixture(),a=f.file();f.set(a);await f.changed();
  const event={text:'<task-notification>'+f.n(a),origin:{kind:'task-notification'}};
  assert.equal((await f.api.prompt(f.io,event,f.next())).drop,undefined);
  f.s.eligible=false;f.s.paused=true;f.s.epoch++;f.api.cancel();
  const row=await f.row(a);
  assert(!row.message.content[0].text.includes(f.n(a)));assert.equal(f.records.length,0);
  f.s.eligible=true;f.s.paused=false;f.s.epoch++;await f.changed();
  assert.deepEqual(f.native,['queue']);assert.equal(f.records.length,0);
  report.cases.push('Esc after pending native prompt cannot fabricate pre-entry refusal or release original head');
}
{
  const f=fixture(),a=f.file();f.set(a);await f.changed();let reached=false;
  const originalRun=f.io.run;f.io.run=async(argv,options)=>{
    const result=await originalRun(argv,options);if(argv.includes('claim'))throw Error('entry result lost');return result;
  };
  await assert.rejects(f.api.prompt(f.io,{text:'<task-notification>'+f.n(a),origin:{kind:'task-notification'}},f.next(async()=>{reached=true;return {};})));
  assert.equal(reached,false);assert.equal(f.records.length,0);
  f.io.run=originalRun;f.reload();await f.changed();assert.deepEqual(f.native,[]);
  report.cases.push('lost pending-prompt entry-helper reply remains one-shot across module reload');
}
report.passed=true;console.log(JSON.stringify(report));
