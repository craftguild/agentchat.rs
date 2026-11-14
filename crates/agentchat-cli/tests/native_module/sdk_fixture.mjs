// SDK simulation only. Real helpers, MCP and SQLite do not admit this fake host.
import { spawn } from 'node:child_process';
import { readFile, writeFile } from 'node:fs/promises';
import { createConnection } from 'node:net';
import { createInterface } from 'node:readline';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { pathToFileURL } from 'node:url';
import { runInputs } from './input_integration.mjs';
import { runResume } from './resume_integration.mjs';
const [modulePath, agentchat, state, config, cwd, scenario] = process.argv.slice(2);
const version=scenario.endsWith('303')?'2.1.303':scenario.endsWith('302')?'2.1.302':'2.1.301';
// ADR 0020 selects the current adapter by its surface, regardless of this identity label.
const fullInput=true;
const earlyScenario=scenario.replace(/30[23]$/,'');
const env = { ...process.env, AGENTCHAT_HOME: state, AGENTCHAT_USER_CONFIG_ROOT: config };
let root = randomUUID(), registeredMarker = null, streamChild = null, closing = false;
let mounted = true, unboundCommands = 0, observedCwd = cwd;
let holdRegistration = null;
let rejectStreams = false, rejectedStreams = 0;
let contextRefreshHint = null;
const children = new Set(), hooks = new Map(), catches = new Map();
// Private test-owned RPC only: departed-root diagnostics are intentionally not
// public actor selection, managed-native admission, Join or presence renewal.
async function privateWhoami(contextId) {
  const fixture=JSON.parse(await readFile(state+'/sdk-fixture-reply.json','utf8'));
  const socket=createConnection(fixture.runtime_socket), lines=createInterface({input:socket});
  const replies=lines[Symbol.asyncIterator]();
  const timer=setTimeout(()=>socket.destroy(Error('private fixture RPC timed out')),10000);
  const call=async(method,params)=>{
    const id=randomUUID();socket.write(JSON.stringify({jsonrpc:'2.0',id,method,params})+'\n');
    const {value,done}=await replies.next();assert.equal(done,false);
    const reply=JSON.parse(value);assert.equal(reply.id,id);assert.equal(reply.error,undefined,JSON.stringify(reply));
    assert.equal(reply.result.error,undefined,JSON.stringify(reply));return reply.result;
  };
  try {
    await call('handshake',{version:1,project_id:fixture.project_id});
    return await call('application.query',{actor:{kind:'native',context_id:contextId},query:{operation:'whoami'}});
  } finally {clearTimeout(timer);lines.close();socket.destroy();}
}
function launch(argv, options = {}) {
  if (closing) throw Error('simulated native host is closing');
  const child = spawn(argv[0], argv.slice(1), { cwd, env: options.env ?? process.env, stdio: ['pipe', 'pipe', 'pipe'] });
  children.add(child); child.once('exit', () => children.delete(child)); return child;
}
async function run(argv, options = {}) {
  if (argv[1]==='internal' && argv[2]==='claude-startup') report.startup_helper_runs=(report.startup_helper_runs??0)+1;
  const child = launch(argv, { env: { ...process.env, ...options.env } });
  const stdout = [], stderr = [];
  child.stdout.on('data', v => stdout.push(v)); child.stderr.on('data', v => stderr.push(v));
  const timer = setTimeout(() => child.kill('SIGTERM'), options.timeoutMs ?? 10000);
  // A refusing helper can exit before reading stdin. The process API still
  // returns that refusal; wait for captured output, not just process exit.
  const code = await new Promise((resolve, reject) => {
    child.stdin.on('error', error => { if (error.code !== 'EPIPE') reject(error); });
    child.once('close', resolve); child.once('error', reject);
    child.stdin.end(options.stdin ?? '');
  }); clearTimeout(timer);
  const result = { exitCode: code, stdout: Buffer.concat(stdout).toString(), stderr: Buffer.concat(stderr).toString() };
  if (code !== 0) console.error('SDK helper status', argv[2], argv[2]==='claude-input'
    ? {exitCode:code,stdout:result.stdout.slice(0,512),stderrBytes:Buffer.byteLength(result.stderr)} : result);
  return result;
}
const $ = {
  env: { get: async name => process.env[name] },
  session: { id: async () => root, cwd: async () => observedCwd,
    version: async () => ({ version }), surfaces: async () => ['terminal'], append: async value => value },
  command: {
    register: async value => {
      assert(mounted, 'native command API requires a mounted session');
      if (scenario === 'early_overlap' && registeredMarker === null) await new Promise(resolve => { holdRegistration = resolve; });
      registeredMarker = value;
    },
    list: async () => {
      if (!mounted) { unboundCommands++; throw Error('native command API requires a mounted session'); }
      return registeredMarker === null ? [] : [registeredMarker];
    },
    run: async () => ({})
  },
  fs: { read: path => path === cwd + '/.agentchat-wakeup'
    ? Promise.resolve(String(signalGeneration) + '\n') : readFile(path, 'utf8') },
  process: {
    run,
    // Deliberately support only the argv shape already used by real probes.
    // Explicit --state-directory keeps stream isolation independent of spawn env.
    spawn: async function* ({ argv }) {
      if (rejectStreams) { rejectedStreams++; throw Error('fixture startup failure'); }
      const child = launch(argv); child.stdin.end(); child.stderr.resume();
      streamChild = child;
      for await (const value of child.stdout) {
        if (contextRefreshHint !== null) {
          const hint=contextRefreshHint;contextRefreshHint=null;
          yield {stream:'stdout',text:JSON.stringify(hint)+'\n'};
        }
        yield { stream: 'stdout', text: value.toString() };
      }
    }
  }
};
function on(name, selector, handler) {
  hooks.set(name, handler ?? selector);
  return { catch: callback => { catches.set(name, callback); } };
}
async function emit(name, event, body = async value => value, options = {}) {
  let caught=false,settled=null;
  const next = value => {
    if(caught && next.called) return settled; // Actual SDK Caught.next replays, including rejection.
    next.called=true;settled=Promise.resolve().then(()=>body(value));return settled;
  };
  next.called = false; next.signal = options.signal ?? new AbortController().signal;
  next.origin = options.origin ?? { plugin: 'engine', tier: 'core' };
  try {
    const result=await hooks.get(name)($,event,next);
    if(name==='session.append' && result===undefined) throw Error('session.append hook returned no result');
    return result;
  }
  catch (error) {
    console.error('SDK hook failure', name, error); if (!catches.has(name)) throw error;
    caught=true;next.error=Object.freeze({kind:'throw',message:error.message,budget:1000});
    return catches.get(name)($, event, next);
  }
}
async function metadata() {
  const result = await run([agentchat, 'internal', 'claude-namespace', '--native-executable', process.execPath,'--native-version',version], { env });
  assert.equal(result.exitCode, 0, result.stderr); return JSON.parse(result.stdout);
}
async function header() { return JSON.parse(await readFile((await metadata()).directory + '/owner-state.json', 'utf8')); }
async function observe() {
  const ns = await metadata(), endpoint = JSON.parse(await readFile(ns.directory + '/fresh-' + ns.observer + '.json', 'utf8'));
  const socket = createConnection(endpoint.socket); let buffer = '';
  return new Promise((resolve, reject) => {
    socket.setTimeout(6000, () => socket.destroy(Error('observation timeout')));
    socket.once('error', reject);
    socket.once('connect', () => socket.write(JSON.stringify({ nonce: randomUUID() }) + '\n'));
    socket.on('data', bytes => { buffer += bytes; if (buffer.includes('\n')) { socket.end(); resolve(JSON.parse(buffer.slice(0, buffer.indexOf('\n')))); } });
    socket.once('end', () => { if (!buffer.includes('\n')) reject(Error('observation ended')); });
  });
}
async function until(predicate) {
  const deadline = Date.now() + 10000;
  while (!await predicate()) { assert(Date.now() < deadline, 'module did not become ready'); await new Promise(resolve => setTimeout(resolve, 10)); }
}
class Mcp {
  constructor() {
    this.child = launch([agentchat, 'mcp'], { env: { ...env, AGENTCHAT_CLAUDE_NATIVE_EXE: process.execPath,AGENTCHAT_CLAUDE_NATIVE_VERSION:version } });
    this.child.stderr.on('data', value => { this.stderr = (this.stderr ?? '') + value; });
    this.lines = createInterface({ input: this.child.stdout })[Symbol.asyncIterator](); this.counter = 0;
  }
  async request(method, params) {
    this.child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: ++this.counter, method, params }) + '\n');
    const value = await this.lines.next(); assert(!value.done); const response = JSON.parse(value.value); assert.equal(response.id, this.counter); return response;
  }
  async start() {
    const response = await this.request('initialize', { protocolVersion: '2025-11-25', capabilities: {}, clientInfo: { name: 'native-sdk-simulation', version: '1' } });
    assert.equal(response.result.protocolVersion, '2025-11-25');
    this.child.stdin.write(JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' }) + '\n');
  }
  async call(name, arguments_) {
    const deadline = Date.now() + 2000;
    for (let attempt = 0; ; attempt++) {
      // Binding restoration may hold the immutable server's lock. Its documented
      // retryable busy is not native readiness loss. Reuse semantic arguments
      // (especially request_id), but obtain a fresh native tool-use proof.
      const id = 'native_' + randomUUID().replaceAll('-', '_');
      const result = await emit('tool.call', { tool: 'mcp__agentchat__' + name, tool_use_id: id }, async () => {
        const response = await this.request('tools/call', { name, arguments: arguments_, _meta: { 'claudecode/toolUseId': id } });
        assert.equal(response.error, undefined); return response.result.structuredContent;
      });
      if (result.error?.code !== 'busy' || result.error.retryable !== true ||
          attempt >= 19 || Date.now() >= deadline) return result;
      (report.binding_busy_retries ??= []).push({ tool: name, request_id: arguments_.request_id ?? null, tool_use_id: id });
      await new Promise(resolve => setTimeout(resolve, 25));
    }
  }
  async close() {
    const done = new Promise(resolve => this.child.once('exit', resolve)); this.child.stdin.end();
    assert.equal(await done, 0, this.stderr); assert(!this.stderr);
  }
}
const report = { sdk_mock: true, actual_native_proof: false, fixture_path: process.argv[1] };
let earlyPending = null, settleEarly = null, earlyAbort = null;
let signalGeneration = 0;
let fixtureSequence = 0;
async function wake(turn = null) {
  const ns = await metadata(), h = await header(), context = h.current.lifecycle.registration.context;
  const sequence = ++fixtureSequence;
  await writeFile(state + '/sdk-fixture-request.json', JSON.stringify({ sequence, context, turn }), { mode: 0o600 });
  let issued;
  await until(async () => { try { issued = JSON.parse(await readFile(state + '/sdk-fixture-reply.json', 'utf8')); return issued.sequence === sequence; } catch { return false; } });
  const value = { root, context_id: context.context_id, incarnation: context.incarnation, generation: sequence, token: issued.token, expected_turn_id: turn, hint: issued.hint };
  await writeFile(ns.directory + '/wake-' + context.context_id + '.json', JSON.stringify(value), { mode: 0o600 });
  // FileChanged and its native fs getter are explicit SDK simulation inputs.
  // The physical counter belongs to the actual signal port and durable intent;
  // changing it directly would fabricate an inconsistent product state.
  signalGeneration = value.generation;
  return { session_id: root, file_path: cwd + '/.agentchat-wakeup' };
}
async function guard(mode, input) {
  return run([agentchat, 'internal', 'claude-guard', '--mode', mode, '--state-directory', state, '--native-executable', process.execPath], { env, stdin: JSON.stringify(input) });
}
async function controls() {
  // Current full-input startup releases its own permitted initial pause. A real
  // cancellation then establishes the independent pause exercised below.
  assert.equal((await observe()).paused, false);
  const initialCancel = new AbortController();
  await emit('tool.call', { tool: 'Bash', tool_use_id: 'initial-control-cancel' }, async () => initialCancel.abort(), { signal: initialCancel.signal });
  assert.equal((await observe()).paused, true);
  await emit('prompt.submit', { text: 'continue', origin: { kind: 'composer' } });
  assert.equal((await observe()).paused, false);
  // Full-body queue receipts and final-permit replay are covered by runInputs,
  // using the current adapter. Legacy agentchat_guard_permit is no longer its route.
  await emit('tool.call', { tool: 'Bash', tool_use_id: 'approval-tool' }, async () => {
    await emit('classic.PermissionRequest', { session_id: root });
    assert.equal((await observe()).approval, true);
  });
  assert.equal((await observe()).approval, false);
  await emit('turn.start', { turnId: 'interrupted-owned-turn' });
  const signal = new AbortController();
  await emit('tool.call', { tool: 'Bash', tool_use_id: 'cancelled-tool' }, async () => signal.abort(), { signal: signal.signal });
  const cancelled = await observe();
  assert.equal(cancelled.paused, true);
  assert.equal(cancelled.pause_cause.code, 'tool_cancelled');
  assert.equal(cancelled.pause_cause.tool, 'Bash');
  assert.equal(cancelled.pause_cause.call_id, 'cancelled-tool');
  assert.equal(cancelled.pause_cause.abort, 'other');
  await emit('turn.complete', { turnId: 'interrupted-owned-turn', isAborted: true, reason: 'aborted' });
  assert.deepEqual((await observe()).pause_cause, cancelled.pause_cause);
  for (const denied of [{ block: 'denied' }, { preventContinuation: true }]) {
    await emit('classic.SessionStart', { session_id: root, source: 'resume' }, async () => denied);
    assert.equal((await observe()).paused, true);
  }
  await emit('classic.SessionStart', { session_id: root, source: 'resume' }, async () => ({}), { origin: { plugin: 'foreign', tier: 'core' } });
  assert.equal((await observe()).paused, true);
  await emit('classic.SessionStart', { session_id: root, source: 'resume' }, async () => ({}));
  const resumed = await observe();
  assert.equal(resumed.paused, false);
  assert.deepEqual(resumed.pause_cause, cancelled.pause_cause);
  await emit('turn.start', { turnId: 'owned-active-turn' });
  assert.deepEqual((await observe()).active_turns, ['owned-active-turn']);
  await emit('turn.complete', { turnId: 'owned-active-turn', isAborted: false, reason: 'complete' });
  assert.deepEqual((await observe()).active_turns, []);
  const stale = 'AGENTCHAT_INPUT_NOTICE root=' + root + ' context=' + randomUUID() + ' token=' + randomUUID() + '\nstale body';
  assert.equal(typeof (await emit('prompt.submit', { text: stale, origin: { kind: 'task-notification' } })).drop, 'string');
  // Labels from other SDK tools must not break the bounded Rust JSON relay or
  // expose exception text when a cancellation reason contains private data.
  const privateAbort = new AbortController();
  const longLabel = 'foreign-x' + '😀'.repeat(100) + '\uD800';
  await emit('tool.call', { tool: longLabel, tool_use_id: 'private-reason-tool' }, async () => privateAbort.abort(Error('do-not-log-this-exception')), { signal: privateAbort.signal });
  const diagnostic = await observe(); assert.equal(diagnostic.paused, true);
  assert.equal(diagnostic.pause_cause.code, 'tool_cancelled');
  assert(diagnostic.pause_cause.tool.length <= 160);
  assert(!/[\uD800-\uDFFF]/.test(diagnostic.pause_cause.tool));
  assert(!JSON.stringify(diagnostic.pause_cause).includes('do-not-log-this-exception'));
  report.controls = { exact_notice_and_final: true, single_use_final: true, approval: true, abort: true, lifecycle_veto: true, core_resume: true, completed_turn_revocation: true };
}
async function recovery(mcp) {
  // A rejected typed input establishes pause; replacement cannot release it.
  await emit('prompt.submit', { text: 'continue', origin: { kind: 'composer' } }, async () => ({ drop: 'cancelled input' }));
  const before = await header(), oldChild = streamChild, mcpPid = mcp.child.pid;
  assert.equal((await observe()).paused, true);
  assert.equal((await observe()).pause_cause.code, 'prompt_dropped');
  assert.equal((await observe()).pause_cause.origin, 'composer');
  assert(oldChild && oldChild.exitCode === null && oldChild.signalCode === null);
  oldChild.kill('SIGTERM');
  await until(async () => streamChild !== oldChild && (await header()).generation > before.generation);
  const after = await observe(); assert.equal(after.available, true); assert.equal(after.paused, true); assert.equal(after.root, root);
  assert(after.native_sequence > before.observation_sequence); assert.equal(mcp.child.pid, mcpPid);
  const status = await mcp.call('agentchat_whoami', { project_path: cwd }); assert.equal(status.error, undefined, JSON.stringify(status));
  report.recovery = { exact_owned_child: true, native_input_required: false, pause_retained: true, physical_mcp_unchanged: true };
}
async function refreshContext(mcp) {
  // SDK-handler simulation only. Separate actual-relay tests authenticate the
  // real supervisor peer; this injection exercises native capture/stale guards.
  await emit('prompt.submit',{text:'continue',origin:{kind:'composer'}},async()=>({drop:'cancelled input'}));
  const before=await header(),prior=before.current.lifecycle.registration.context.context_id,pid=mcp.child.pid;
  const hint={event:'context_refresh',context_id:prior,native_generation:before.generation};
  contextRefreshHint=hint;await observe();
  const after=await header();assert(after.generation>before.generation);
  assert.notEqual(after.current.lifecycle.registration.context.context_id,prior);
  assert.equal((await observe()).paused,true);assert.equal(mcp.child.pid,pid);
  contextRefreshHint=hint;await observe();assert.equal((await header()).generation,after.generation);
  contextRefreshHint={...hint,context_id:randomUUID(),native_generation:after.generation};
  await observe();assert.equal((await header()).generation,after.generation);
  const priorRoot=root;root=randomUUID();
  contextRefreshHint={...hint,context_id:after.current.lifecycle.registration.context.context_id,native_generation:after.generation};
  assert.equal((await observe()).available,false);assert.equal((await header()).generation,after.generation);root=priorRoot;
  assert.equal((await observe()).available,true);
  report.context_refresh={fresh_native_context:true,physical_mcp_unchanged:true,pause_retained:true,stale_and_cross_root_hints_inert:true};
}
async function exhaustedStream(mcp) {
  await emit('prompt.submit', { text: 'continue', origin: { kind: 'composer' } }, async () => ({ drop: 'cancelled input' }));
  const old = streamChild, before = await header(), mcpPid = mcp.child.pid;
  // Age the real child beyond the supervisor's immediate-failure window, then
  // exhaust exactly its existing three attempts. No replacement is available.
  await new Promise(resolve => setTimeout(resolve, 210));
  rejectStreams = true; old.kill('SIGTERM');
  await until(async () => rejectedStreams === 3);
  await assert.rejects(observe());
  rejectStreams = false;
  const status = await mcp.call('agentchat_whoami', { project_path: cwd });
  assert.equal(status.error, undefined, JSON.stringify(status));
  const after = await observe();
  assert.equal(after.available, true); assert.equal(after.paused, true);
  assert.equal(after.root, root); assert.equal(mcp.child.pid, mcpPid);
  assert.notEqual(streamChild, old); assert.equal(rejectedStreams, 3);
  assert((await header()).generation > before.generation);
  report.exhausted_stream = { ordinary_native_call_retries_supervisor: true, pause_retained: true, physical_mcp_unchanged: true };
}
try {
  const { register } = await import(pathToFileURL(modulePath)); register(on);
  if (['capability302','capability_spawn302','capability_env302','capability_input_cwd302'].includes(scenario)) {
    const background = scenario === 'capability_spawn302';
    const promptApi = scenario === 'capability_env302';
    const inputApi = scenario === 'capability_input_cwd302';
    const startupApi = !background && !promptApi && !inputApi;
    const missing = background ? 'process.spawn' : promptApi ? 'env.get' : inputApi ? 'session.cwd' : 'session.id';
    let attempted = false;
    if (background) Object.defineProperty($.process, 'spawn', { get: () => { attempted = true; return undefined; } });
    else if (promptApi) delete $.env.get; else if (startupApi) delete $.session.id;
    await emit('session.start', { isInteractive: true, surface: 'terminal' });
    // Observe the actual later API access; an initially empty child set is no
    // readiness proof. This getter belongs only to the explicit SDK simulation.
    if (background) await until(async () => attempted);
    if (inputApi) {
      await until(async () => (await observe()).available);
      await emit('classic.SessionStart', {session_id:root,source:'startup'});
      assert.equal((await header()).current.lifecycle.stage,'project');
      assert.equal((await observe()).paused,false,'Exercise an eligible input callback, not a startup short circuit');
      Object.defineProperty($.session,'cwd',{get:()=>{attempted=true;return undefined;}});
      await emit('classic.FileChanged', { file_path: cwd + '/.agentchat-wakeup', session_id: root });
      assert.equal(attempted,true,'The input callback must reach the missing API');
    }
    if (background || startupApi) await until(async () => children.size === 0);
    let processed = 0;
    const refused = await emit('prompt.submit', { text: 'load the AgentChat skill', origin: { kind: 'composer' } }, async () => { processed++; });
    assert.equal(processed, 0, 'An observed required API failure must block the affected prompt');
    assert.equal(refused?.drop, 'AgentChat native capability unavailable: ' + missing);
    if (startupApi) assert.equal(registeredMarker, null);
    if (background || startupApi) { assert.equal(streamChild, null); assert.equal(children.size, 0); }
    else assert.notEqual(streamChild, null);
    report.capability_failure = { stage: background ? 'later_api_use' : promptApi ? 'prompt_api_use' : inputApi ? 'input_callback' : 'before_registration', used_api: missing, affected_prompt_blocked: true, visible_error: refused.drop };
    report.passed = true;
  } else {
  if (scenario.startsWith('early_')) {
    mounted = false;
    const source = earlyScenario.startsWith('early_startup') ? 'startup' : 'resume';
    const result = earlyScenario.endsWith('_block') ? { block: 'denied' } : earlyScenario.endsWith('_stop') ? { preventContinuation: true } : {};
    const abort = new AbortController(); if (['early_abort','early_startup_abort'].includes(earlyScenario)) abort.abort();
    const origin = { plugin: earlyScenario.endsWith('_foreign') ? 'foreign' : 'engine', tier: 'core' };
    if (earlyScenario.endsWith('_late_abort')) {
      earlyAbort = abort;
      earlyPending = emit('classic.SessionStart', { session_id: root, source }, async () => {
        await new Promise(resolve => { settleEarly = resolve; }); return result;
      }, { signal: abort.signal, origin });
      await until(async () => settleEarly !== null);
    } else assert.deepEqual(await emit('classic.SessionStart', { session_id: root, source }, async () => result, { signal: abort.signal, origin }), result);
    assert.equal(unboundCommands, 0, 'early classic callback reached the unmounted native command API');
    assert.equal(registeredMarker, null);
    mounted = true;
  }
  // Start the module before MCP to exercise the first-endpoint race.
  if (scenario === 'early_overlap') {
    const starting = emit('session.start', { isInteractive: true, surface: 'terminal' });
    await until(async () => holdRegistration !== null);
    await emit('classic.SessionStart', { session_id: root, source: 'resume' }, async () => ({}));
    holdRegistration(); await starting;
  } else await emit('session.start', { isInteractive: true, surface: 'terminal' });
  if (earlyPending !== null) { earlyAbort.abort(); settleEarly(); await earlyPending; }
  // The controls case has an explicitly synthetic project runtime before native
  // startup. Other cases retain the uninitialized bootstrap/promotion path.
  const initialStage = (scenario==='controls'||scenario.startsWith('inputs')) ? 'project' : 'bootstrap';
  await until(async () => (await header()).current?.lifecycle?.stage === initialStage);
  const before = await header(); assert.equal(before.current.lifecycle.stage, initialStage);
  const mcp = new Mcp(); await mcp.start();
  await until(async () => (await header()).current?.reservation.transport_nonce != null);
  if (scenario.startsWith('early_')) {
    const snapshot = await observe();
    assert.equal(snapshot.available, true);
    assert.equal(snapshot.paused, !(['early_resume', 'early_overlap'].includes(earlyScenario) || (fullInput && earlyScenario==='early_startup')));
    report.early_order = { commands_before_mount: unboundCommands, source: scenario, ready: true, paused: snapshot.paused };
    if (fullInput) {
      await emit('prompt.submit',{text:'cancelled original input',origin:{kind:'composer'}},async()=>({drop:'fixture cancellation'}));
      assert.equal((await observe()).paused,true);
      await emit('classic.SessionStart',{session_id:root,source:'startup'},async()=>({}));
      assert.equal((await observe()).paused,true,'a startup replay cannot undo cancellation or a prior lifecycle veto');
      report.startup_replay_retains_pause=true;
    }
  }
  await emit('classic.SessionStart', { session_id: root, source: 'startup' }, async () => ({}));
  if (scenario==='startup302') assert.equal((await observe()).paused,false);
  const status = await mcp.call(initialStage==='bootstrap'?'agentchat_doctor':'agentchat_whoami', { project_path: cwd });
  assert.notEqual(status.error?.code, 'session_required', JSON.stringify(status));
  report.bootstrap_status = status;
  const key = randomUUID(); const initialized = await mcp.call('agentchat_init', { project_path: cwd, request_id: key });
  assert.equal(initialized.error, undefined, JSON.stringify(initialized)); report.initialized = initialized;
  const bound = await header(); assert.equal(bound.current.lifecycle.stage, 'project'); assert.equal(bound.current.lifecycle.registration.root, root);
  const replay = await mcp.call('agentchat_init', { project_path: cwd, request_id: key });
  assert.equal(replay.error, undefined, JSON.stringify(replay));
  if (scenario === 'cwd_fence') {
    assert.equal((await observe()).available, true); observedCwd = cwd + '/changed';
    assert.equal((await observe()).available, false); observedCwd = cwd;
    assert.equal((await observe()).available, true); report.cwd_fence = true;
  }
  if (scenario === 'controls') await controls();
  if (scenario.startsWith('inputs')) await runInputs({scenario,$,emit,run,metadata,header,until,mcp,agentchat,state,cwd,env,root,report,privateWhoami});
  if (scenario === 'recovery') await recovery(mcp);
  if (scenario === 'refresh302') await refreshContext(mcp);
  if (scenario === 'stream_exhaustion') await exhaustedStream(mcp);
  if (scenario.startsWith('resume_')) await runResume({scenario,$,emit,header,observe,getRoot:()=>root,setRoot:id=>{root=id;},report});
  if (scenario==='fault_stage302') {
    const other=randomUUID();
    let reads=0;
    const originalId=$.session.id;
    $.session.id=async()=>{
      if (++reads===2) {
        const inFlight=await observe();
        assert.equal(inFlight.available,false,'diagnostic getter cannot delay the failure latch');
        assert.equal(inFlight.paused,true);
      }
      return originalId();
    };
    await emit('classic.SessionStart',{session_id:other,source:'resume'},async()=>({}));
    $.session.id=originalId;
    const snapshot=await observe();
    assert.equal(snapshot.available,false);assert.equal(snapshot.paused,true);
    assert.equal(snapshot.pause_cause.failure,'lifecycle_root_disagreement');
    assert.equal(snapshot.pause_cause.failure_kind,'throw');
    assert.equal(snapshot.pause_cause.event_root,other);assert.equal(snapshot.pause_cause.observed_root,root);
    report.fixed_fault_stage=true;
  } else {
  const priorGeneration = bound.generation, priorContext = bound.current.lifecycle.registration.context.context_id;
  const previousRoot = root; root = randomUUID();
  await emit('classic.SessionStart', { session_id: root, source: 'clear' }, async () => ({}));
  const cleared = await header(); assert(cleared.generation > priorGeneration); assert.notEqual(cleared.current.lifecycle.registration.context.context_id, priorContext);
  assert.equal(cleared.current.lifecycle.registration.root, root);
  if (scenario.startsWith('inputs')) {
    const launched=JSON.parse(await readFile(state+'/sdk-fixture-reply.json','utf8'));
    const proofPath=state+'/hosts/launches/root-changes/'+launched.launch_generation+'.json';
    const proofBytes=await readFile(proofPath,'utf8'),changed=JSON.parse(proofBytes);
    assert.equal(changed.new_root,root);assert.equal(changed.binding.context.host_session_id,previousRoot);
    await until(async()=>{
      const status=await privateWhoami(priorContext);
      return status.result.participation==='left' && status.result.native_delivery.state==='cancelled';
    });
    report.root_retirement={original_process_still_alive:true,positive_native_fact:true,retired:true};
    const switchedRoot=root;root=previousRoot;
    await emit('classic.SessionStart',{session_id:root,source:'clear'},async()=>({}));
    const returned=await header();
    assert.notEqual(returned.current.lifecycle.registration.context.context_id,priorContext);
    assert.equal(await readFile(proofPath,'utf8'),proofBytes);
    const prior=await privateWhoami(priorContext);
    assert.equal(prior.result.participation,'left');
    report.root_retirement.return_does_not_revive=true;
    root=switchedRoot;
    await emit('classic.SessionStart',{session_id:root,source:'clear'},async()=>({}));
    assert.equal(await readFile(proofPath,'utf8'),proofBytes);
  }
  const conflict = await mcp.call('agentchat_init', { project_path: cwd, request_id: key });
  assert.equal(conflict.error?.code, 'idempotency_conflict', JSON.stringify(conflict));
  report.root_isolation = { previous: previousRoot, current: root, context_changed: true };
  }
  await emit('classic.SessionEnd', { session_id: root });
  assert.equal((await header()).retired, true); await mcp.close(); report.passed = true;
  }
} finally {
  closing = true; registeredMarker = null;
  const owned = [...children];
  await Promise.all(owned.map(child => new Promise(resolve => { child.once('exit', resolve); child.kill('SIGTERM'); })));
}
console.log(JSON.stringify(report));
