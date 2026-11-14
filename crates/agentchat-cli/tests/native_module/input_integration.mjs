// Actual Rust CLI/native provenance/runtime/SeaORM paths; SDK and admission are simulated.
import assert from 'node:assert/strict';
import { readFile,writeFile } from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
export async function runInputs({scenario,$,emit,run,metadata,header,until,mcp,agentchat,state,cwd,env,root,report,privateWhoami}) {
  const ns=await metadata(),context=(await header()).current.lifecycle.registration.context;
  const appended=[];
  const ambiguous=scenario==='inputs_append_unknown302', queueUnknown=scenario==='inputs_queue_unknown302';
  const negativeRecords=[];const originalRun=$.process.run;
  if(ambiguous||queueUnknown)$.process.run=async(argv,options={})=>{
    if(argv[2]==='claude-input'&&argv.includes('record')){
      const request=JSON.parse(options.stdin);if(['failed','rejected_before_entry'].includes(request.native?.route))negativeRecords.push(request);
    }
    return originalRun(argv,options);
  };

  $.session.append=async ({message})=>{
    const uuid=randomUUID(),kept={...message,role:'user',isMeta:true};
    const result=await emit('session.append',{uuid,message:kept,door:'append',origin:{kind:'engine'}},async e=>({uuid,message:e.message}));
    appended.push(result);if(ambiguous)throw Error('Simulated kept append with lost return');return result;
  };
  const generation=randomUUID();process.env.AGENTCHAT_LAUNCH_ROOT=root;process.env.AGENTCHAT_LAUNCH_GENERATION=generation;
  let entered=0;
  const original='first original user input before startup commit';
  const held=emit('prompt.submit',{text:original,origin:{kind:'composer'}},async e=>{entered++;assert.equal(e.text,original);return {text:e.text};});
  const secondOriginal='second original input in the same startup wait';
  const alsoHeld=emit('prompt.submit',{text:secondOriginal,origin:{kind:'composer'}},async e=>{entered++;assert.equal(e.text,secondOriginal);return {text:e.text};});
  await new Promise(resolve=>setTimeout(resolve,150));
  assert.equal(entered,0,'uncommitted startup must hold the original first input');
  await writeFile(state+'/sdk-fixture-request.json',JSON.stringify({sequence:1,context,generation,owner:(await header()).owner}),{mode:0o600});
  await until(async()=>{try{return JSON.parse(await readFile(state+'/sdk-fixture-reply.json','utf8')).sequence===1;}catch{return false;}});
  const posted=JSON.parse(await readFile(state+'/sdk-fixture-reply.json','utf8')).posted;
  const frame=value=>{
    const lines=value.file.text.split('\n'), message=lines[0]==='[AgentChat:Message]';
    assert.equal(lines[0],message?'[AgentChat:Message]':'[AgentChat:Notice]');
    assert.equal(lines[message?2:1],'--');
    const metadata=JSON.parse(lines[message?1:2]);
    assert.equal(value.file.version,5);assert.equal(metadata.frame_revision,'dispatch_v4');
    assert.equal(Object.hasOwn(metadata,"auto_ack"),false);assert.equal(metadata.auto_read,true);
    if(metadata.message_id){assert.equal(metadata.author.kind,'agent');assert.equal(metadata.dispatch_id,metadata.message_id);}
    return metadata;
  };
  const automaticReceipt=async(messageId,actorId)=>{
    const result=await mcp.call('agentchat_message',{project_path:cwd,message_id:messageId});
    assert.equal(result.error,undefined,JSON.stringify(result));
    const rows=result.result.receipts.items.filter(row=>row.recipient.actor_id===actorId);
    assert.equal(rows.length,1);
    assert(rows[0].delivery.delivered_at);assert.equal(Object.hasOwn(rows[0].delivery,"acknowledged_at"),false);
    assert.deepEqual(rows[0].reads.map(read=>read.kind),['automatic']);
    assert.equal(rows[0].reads[0].at,rows[0].delivery.delivered_at);
  };
  assert.equal((await held).drop,undefined);assert.equal((await alsoHeld).drop,undefined);assert.equal(entered,2);
  assert.equal(report.startup_helper_runs,1);
  report.first_prompt={held_before_commit:true,released_after_shared_start:true,each_original_once:true,one_shared_helper:true};
  const pointerPath=ns.directory+'/pending-input-'+context.context_id+'.json';
  const head=async()=>{
    const pending=JSON.parse(await readFile(pointerPath,'utf8'));
    return {pending,file:JSON.parse(await readFile(ns.directory+'/input-'+context.context_id+'-'+pending.token+'.json','utf8'))};
  };
  let first;
  await until(async()=>{try{first=await head();return true;}catch{return false;}});
  frame(first);
  const event={session_id:root,file_path:cwd+'/.agentchat-wakeup',hook_event_name:'FileChanged'};
  const permitted=await emit('classic.FileChanged',event);assert(permitted.agentchat_input_permit);
  const guard=(mode,input)=>run([agentchat,'internal','claude-input','--mode',mode,'--state-directory',state,'--native-executable',process.execPath],{env,stdin:JSON.stringify(input)});
  const notice=await guard('notice',permitted);assert.equal(notice.exitCode,2,notice.stderr);
  assert(notice.stderr.includes(first.file.text));assert(notice.stderr.includes('\0'.repeat(65_500)));
  for(let i=0;i<3;i++){assert.equal((await emit('classic.FileChanged',event)).agentchat_input_permit,undefined);}
  assert.equal(appended.length,0);assert.equal((await head()).pending.token,first.pending.token);
  const uuid=randomUUID(),message={type:'user',role:'user',content:[{type:'text',text:notice.stderr.trimEnd()}]};
  const promptFirst=scenario==='inputs_prompt_first302';
  const prompt='<task-notification>\n'+notice.stderr.trimEnd()+'\n</task-notification>';
  if(promptFirst){
    const proof=JSON.parse((await guard('load',{root,generation:first.file.native_generation})).stdout).proof;
    const forged={session_id:root,prompt,agentchat_input_final_permits:[{...permitted.agentchat_input_permit,issued:Date.now(),queue:null,head:proof}]};
    assert.equal((await guard('final',forged)).exitCode,2,'A head permit requires the durable one-shot intent');
    const before=await mcp.call('agentchat_whoami',{project_path:cwd});assert.equal(before.error,undefined);
    const result=await emit('prompt.submit',{text:prompt,origin:{kind:'task-notification'}},async()=>{
      const final=await emit('classic.UserPromptSubmit',{session_id:root,prompt});
      const permit=final.agentchat_input_final_permits[0];assert.equal(permit.queue,null);assert(permit.head);
      const checked=await guard('final',final);assert.equal(checked.exitCode,0,checked.stderr);
      const bad=structuredClone(final);bad.agentchat_input_final_permits[0].head.bytes--;
      assert.equal((await guard('final',bad)).exitCode,2,'Partial head proof must fail');
      const health=await mcp.call('agentchat_whoami',{project_path:cwd});assert.equal(health.error,undefined);
      assert.equal(health.result.native_delivery.settled_seq,before.result.native_delivery.settled_seq);
      assert.equal(Object.hasOwn(health.result.statistics,"acknowledged_deliveries"),false);
      assert.equal((await head()).pending.token,first.pending.token);
      await assert.rejects(readFile(ns.directory+'/accepted-input-'+context.context_id+'-'+first.pending.token+'.json'),e=>e.code==='ENOENT');
      return {text:prompt};
    });
    assert.equal(result.drop,undefined);assert.equal((await guard('notice',permitted)).exitCode,0,'Entered head cannot emit the body again');
    report.prompt_first={one_shot_before_prompt:true,no_invented_row_uuid:true,no_receipt_cursor_or_ack_before_row:true};
  }
  if(queueUnknown){
    const before=await mcp.call('agentchat_whoami',{project_path:cwd});assert.equal(before.error,undefined);
    const kept=[];
    await assert.rejects(emit('session.append',{uuid,message,door:'queue',origin:{kind:'engine'}},async e=>{
      kept.push({uuid,message:e.message});throw Error('Simulated native queue storage with lost return');
    }),/Simulated native queue storage with lost return/);
    assert.equal(kept.length,1);assert.equal(kept[0].message.content[0].text,notice.stderr.trimEnd());
    assert.equal(negativeRecords.length,0,'A stored queue row cannot be reported as definite rejection');
    const receipt=ns.directory+'/accepted-input-'+context.context_id+'-'+first.pending.token+'.json';
    await assert.rejects(readFile(receipt),error=>error.code==='ENOENT');
    let processed=0;
    const late=await emit('prompt.submit',{text:'<task-notification>\n'+notice.stderr.trimEnd()+'\n</task-notification>',origin:{kind:'task-notification'}},async()=>{processed++;return {};});
    assert(late.drop);assert.equal(processed,0);
    const health=await mcp.call('agentchat_whoami',{project_path:cwd});assert.equal(health.error,undefined,'result uncertainty does not disable independent native-bound diagnostics');
    for(let i=0;i<5;i++){
      await Promise.all([emit('classic.FileChanged',event),emit('classic.Stop',{session_id:root})]);
      const status=await privateWhoami(context.context_id);assert.equal(status.error,undefined,JSON.stringify(status));
      assert.equal(status.result.native_delivery.settled_seq,before.result.native_delivery.settled_seq);
      assert.equal(Object.hasOwn(status.result.statistics,"acknowledged_deliveries"),false);assert.equal(status.result.statistics.read_receipts,0);
      assert.equal((await head()).pending.token,first.pending.token);assert.equal(appended.length,0);assert.equal(kept.length,1);
    }
    const blocked=await guard('load',{root,generation:first.file.native_generation});assert.equal(blocked.exitCode,0,blocked.stderr);assert.equal(JSON.parse(blocked.stdout).file,null);
    assert.equal(negativeRecords.length,0);report.ambiguous_queue={stored_once:true,no_negative_receipt:true,no_cursor_or_ack:true,head_and_fifo_retained:true,late_processing_blocked:true};
    report.launched_context=context;return;
  }
  await emit('session.append',{uuid,message,door:'queue',origin:{kind:'engine'}},async e=>({uuid,message:e.message}));
  try {
    const accepted=JSON.parse(await readFile(ns.directory+'/accepted-input-'+context.context_id+'-'+first.pending.token+'.json','utf8'));
    assert.equal(accepted.accepted,true);
  } catch(error) {if(error.code!=='ENOENT')throw error;} // A committed successor may already have retired the receipt.
  let second;
  await until(async()=>{try{second=await head();return second.pending.token!==first.pending.token;}catch{return false;}});
  // Native queue continuation must remain valid even after the application has
  // accepted the first body and retired its payload/receipt for this successor.
  await assert.rejects(readFile(ns.directory+'/input-'+context.context_id+'-'+first.pending.token+'.json'));
  if(!promptFirst)await emit('prompt.submit',{text:prompt,origin:{kind:'task-notification'}},async()=>{
    const final=await emit('classic.UserPromptSubmit',{session_id:root,prompt});
    assert.equal(final.agentchat_input_final_permits.length,1);
    const result=await guard('final',final);assert.equal(result.exitCode,0,result.stderr);return {text:prompt};
  });
  else assert.equal(typeof(await emit('prompt.submit',{text:prompt,origin:{kind:'task-notification'}},async()=>{throw Error('Duplicate original prompt');})).drop,'string');
  assert.equal((await guard('final',{session_id:root,prompt})).exitCode,2);
  if(ambiguous)await Promise.all([emit('classic.FileChanged',event),emit('classic.FileChanged',event),emit('classic.Stop',{session_id:root})]);
  else await emit('classic.FileChanged',event);
  if(ambiguous){
    assert.equal(negativeRecords.length,0,'A kept body with a lost return cannot publish a definite rejection');
    assert.equal(appended.length,1,'Repeated/concurrent hooks must not append the unknown head again');
    assert.equal((await head()).pending.token,second.pending.token);
    for(let index=0;index<5;index++){
      await emit('classic.FileChanged',event);await emit('classic.Stop',{session_id:root});
      const status=await mcp.call('agentchat_whoami',{project_path:cwd});assert.equal(status.error,undefined,JSON.stringify(status));
      assert.equal(status.result.native_delivery.settled_seq,posted[0].dispatch_seq);
      assert.equal(Object.hasOwn(status.result.statistics,"acknowledged_deliveries"),false);assert.equal(status.result.statistics.read_receipts,1);
      await automaticReceipt(posted[0].message_id,status.result.actor.actor_id);
      assert.equal((await head()).pending.token,second.pending.token);assert.equal(appended.length,1);
    }
    const blocked=await guard('load',{root,generation:second.file.native_generation});assert.equal(blocked.exitCode,0,blocked.stderr);assert.equal(JSON.parse(blocked.stdout).file,null);
    report.ambiguous_append={kept_body_once:true,no_negative_receipt:true,no_cursor_or_ack:true,head_and_fifo_retained:true};
    report.launched_context=context;return;
  }
  assert.equal(appended.length,1);assert.equal(appended[0].message.content[0].text,second.file.text);
  let third;
  await until(async()=>{try{third=await head();return third.pending.token!==second.pending.token;}catch{return false;}});
  assert.equal((await emit('classic.FileChanged',event)).agentchat_input_permit.token,third.pending.token);
  const thirdNotice='AGENTCHAT_INPUT_NOTICE root='+root+' context='+context.context_id+' token='+third.pending.token+'\n'+third.file.text;
  const id=randomUUID();await emit('session.append',{uuid:id,message:{type:'user',role:'user',content:[{type:'text',text:thirdNotice}]},door:'queue',origin:{kind:'engine'}},async e=>({uuid:id,message:e.message}));
  await writeFile(state+'/sdk-fixture-native-notice.json','{}',{mode:0o600});
  let mixed;
  await until(async()=>{try{mixed=JSON.parse(await readFile(state+'/sdk-fixture-native-notice-ready.json','utf8'));return true;}catch{return false;}});
  let fourth;
  await until(async()=>{try{fourth=await head();return fourth.pending.token!==third.pending.token;}catch{return false;}});
  assert.ok(fourth.file.text.startsWith('[AgentChat:Notice]\n--\n'));
  const noticeFrame=frame(fourth);
  assert.equal(noticeFrame.notice.kind,'reservation.acquired');
  assert.equal(noticeFrame.dispatch.kind,'notice');
  assert.equal(noticeFrame.notice.dispatch_id,noticeFrame.dispatch.id);
  assert.equal(Object.hasOwn(noticeFrame,'event'),false);
  const notMessage=await mcp.call('agentchat_message',{project_path:cwd,message_id:noticeFrame.dispatch.id});
  assert.equal(notMessage.error?.code,'not_found',JSON.stringify(notMessage));
  assert.equal((await head()).pending.token,fourth.pending.token,'A Notice lookup cannot settle its original native head');
  const loadedNotice=await guard('load',{root,generation:fourth.file.native_generation});
  assert.equal(loadedNotice.exitCode,0,loadedNotice.stderr);
  assert.equal(JSON.parse(loadedNotice.stdout).file.token,fourth.pending.token,'Native-only fence must retain a non-message head');
  await emit('classic.FileChanged',event);
  assert.equal(appended.length,2);assert.equal(appended[1].message.content[0].text,fourth.file.text);
  let fifth;
  await until(async()=>{try{fifth=await head();return fifth.pending.token!==fourth.pending.token;}catch{return false;}});
  await emit('classic.FileChanged',event);
  assert.equal(appended.length,3);assert.equal(appended[2].message.content[0].text,fifth.file.text);
  assert.equal(frame(fifth).message_id,mixed.successor.message_id);
  // A later real control event leaves already accepted input eligible for no
  // automatic resend, even when it will never reach a subsequent prompt.
  await emit('turn.start',{turnId:'sdk-esc-after-queue'});
  const abort=new AbortController();await emit('tool.call',{tool:'Bash',tool_use_id:'sdk-esc'},async()=>abort.abort(),{signal:abort.signal});
  await emit('turn.complete',{turnId:'sdk-esc-after-queue',isAborted:true,reason:'aborted'});
  await until(async()=>{
    const result=await mcp.call('agentchat_whoami',{project_path:cwd});
    await writeFile(state+'/input-status-diagnostic.json',JSON.stringify(result),{mode:0o600});
    assert.equal(result.error,undefined,JSON.stringify(result));
    const binding=result.result;
    const native=binding.native_delivery;
    // Status shape is retained for diagnostics if an implementation changes it.
    report.input_status=result;return native?.state==='ready';
  });
  const count=appended.length;
  const frames=[first,second,third].map(value=>{
    assert.ok(value.file.text.startsWith('[AgentChat:Message]\n'));
    return frame(value);
  });
  frame(fourth);frame(fifth);
  assert.deepEqual(frames.map(value=>value.seq),posted.map(value=>value.dispatch_seq));
  assert.deepEqual(frames.map(value=>value.message_id),posted.map(value=>value.message_id));
  assert.equal(report.input_status.result.native_delivery.settled_seq,mixed.successor.dispatch_seq);
  assert.equal(Object.hasOwn(report.input_status.result.statistics,"acknowledged_deliveries"),false);
  assert.equal(report.input_status.result.statistics.read_receipts,4);
  for(const message of [...posted,mixed.successor]){
    await automaticReceipt(message.message_id,report.input_status.result.actor.actor_id);
  }
  await emit('classic.FileChanged',event);assert.equal(appended.length,count);
  assert.equal((await head()).pending.token,fifth.pending.token);
  report.full_input={synthetic_admission:true,whole_maximum_body:true,awaiting_blocks_passive:true,
    native_queue_receipt:true,passive_exact:true,final_after_head_clear:true,postaccept_esc_no_replay:true,
    notice_and_successor_exact_fifo:true};
  report.launched_context=context;
}
