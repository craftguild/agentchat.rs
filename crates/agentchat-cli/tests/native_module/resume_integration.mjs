// Exact native ordering reproduced with SDK-only getters; helpers remain real.
import assert from 'node:assert/strict';
import {randomUUID} from 'node:crypto';
export async function runResume({scenario,$,emit,header,observe,getRoot,setRoot,report}) {
  const previous=getRoot(), target=randomUUID(), initial=await header();
  const abort=new AbortController(), originalRun=$.process.run;
  let abortedDuringRegistration=false;
  if (scenario==='resume_abort302') $.process.run=async(argv,options)=>{
    const result=await originalRun(argv,options);
    if(argv[2]==='claude-control') {
      const call=JSON.parse(options.stdin);
      if(call.action==='register' && call.input.reservation.root===target) {
        abortedDuringRegistration=true;abort.abort();
      }
    }
    return result;
  };
  try {
    await emit('command.run',{command:'resume',args:target},async()=>{
      await emit('classic.SessionEnd',{session_id:previous});
      await emit('session.end',{sessionId:previous,reason:'resume'});
      const result=scenario==='resume_veto302'?{block:'fixture veto'}:{};
      await emit('classic.SessionStart',{session_id:target,source:'resume'},async()=>result,
        {signal:abort.signal,origin:{plugin:scenario==='resume_foreign302'?'foreign':'engine',tier:'core'}});
      const waiting=await observe(), retained=await header();
      assert.equal(waiting.available,false);assert.equal(waiting.paused,true);
      assert.equal(retained.retired,true);assert.equal(retained.generation,initial.generation);
      assert.equal(retained.current.lifecycle.registration.root,previous);
      // Native 302 updates the SDK root only after classic hooks have returned.
      setRoot(target);return {text:'native command completed'};
    });
  } finally {$.process.run=originalRun;}
  const after=await observe(), changed=await header();
  assert.equal(after.available,true);assert.equal(after.root,target);
  assert.equal(after.paused,scenario!=='resume_order302');
  assert.equal(changed.current.lifecycle.registration.root,target);
  assert(changed.generation>initial.generation);
  if(scenario==='resume_abort302') assert.equal(abortedDuringRegistration,true);
  report.ordered_resume={unbound_while_sdk_lags:true,live_root_required:true,
    registered_after_command:true,pause_preserved:after.paused,actual_native:false};
}
