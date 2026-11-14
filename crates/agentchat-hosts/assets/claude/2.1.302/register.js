import { createInputs } from './input.js';
// Physical locators never identify a root. Managed versions are launch-scoped.
const AGENTCHAT = __AGENTCHAT_EXECUTABLE__, NATIVE = __AGENTCHAT_NATIVE_EXECUTABLE__, ENV = __AGENTCHAT_ENVIRONMENT__;
let VERSION = __AGENTCHAT_VERSION__;
const INSTANCE = Date.now().toString(36) + '-' + Math.random().toString(36).slice(2, 10);
const MARKER = 'agentchat-native-guard', DESCRIPTION = 'AgentChat native guard ' + INSTANCE;
let location = null, nativeRoot = null, generation = 0, binding = null, ready = false;
let current = null, paused = true, faulted = false, epoch = 0, serial = 0;
let pending = null, finalPermit = null, desired = null, worker = null, streamTask = null;
let rootEpoch = 0;
let startupReleased = false;
let startupWork = null;
let startupWaiters = 0;
let reconnectTask = null, reconnectWanted = null, reconnectSeen = null;
let interactive = false, phasePending = false, phaseBusy = false;
let started = false, earlyStart = null;
let initialStart = true;
let ended = null, resuming = null;
let pauseCause = null, capabilityError = null;
const turns = new Map(), turnEpoch = new Map(), calls = new Set(), approvals = new Set();
const inputs = createInputs({ AGENTCHAT, NATIVE, ENV,
  state: () => ({ root: current, nativeRoot, generation, epoch, revision: rootEpoch,
    eligible: eligible(current), paused, context: binding?.stage === 'project' ? binding.registration.context : null }) });
// Keep every SDK call visible to the native same-file static analyzer. The
// imported input coordinator receives only these ordinary operation closures.
function inputIo($) {
  return { run: async (argv, options) => { try { return await $.process.run(argv, options); } catch (error) { capabilityFailure(error.message); throw error; } },
    id: async () => { try { return await $.session.id(); } catch (error) { capabilityFailure(error.message); throw error; } }, cwd: async () => { try { return await $.session.cwd(); } catch (error) { capabilityFailure(error.message); throw error; } },
    append: async message => { try { return await $.session.append(message); } catch (error) { capabilityFailure(error.message); throw error; } }, marker: async () => { try { return await marker($); } catch (error) { capabilityFailure(error.message); throw error; } } };
}
function recordPause(code, details = {}, signal) {
  // Initial native startup may release only its own untouched pause. Later
  // lifecycle replay cannot clear an interruption or a different root's pause.
  if (code !== 'session_start') initialStart = false;
  // Keep the initiating cause through a cascading tool/turn abort. This is
  // bounded historical diagnostics, never an input to eligibility or identity.
  if (paused && pauseCause?.root === current && !['guard_fault', 'epoch_exhausted'].includes(code)) return;
  try {
    const safe = {};
    for (const [key, value] of Object.entries(details)) {
      if (typeof value === 'string') safe[key] = value.slice(0, 160).replace(/[\uD800-\uDFFF]/g, '?');
      else if (typeof value === 'boolean' || Number.isSafeInteger(value)) safe[key] = value;
    }
    // Do not expose arbitrary exception text, prompts or tool arguments.
    const abort = signal?.aborted ? (String(signal.reason) === 'HooksError: user-cancel' ? 'user-cancel' : 'other') : null;
    pauseCause = { code, root: current, generation, epoch, root_epoch: rootEpoch, unix: Date.now(), ...safe, abort };
  } catch { pauseCause = { code: 'unavailable' }; }
}
function bump() {
  inputs.cancel();
  if (!Number.isSafeInteger(epoch + 1)) { recordPause('epoch_exhausted'); faulted = true; paused = true; return; }
  epoch++;
}
function bumpRoot() { if (!Number.isSafeInteger(rootEpoch + 1)) { fail(); return rootEpoch; } return ++rootEpoch; }
function fail(details = {}) { recordPause('guard_fault', details); faulted = true; paused = true; ready = false; pending = null; finalPermit = null; earlyStart = null; ended = null; resuming = null; bump(); }
// Inspect only a definite failed ordinary SDK call. The native analyzer forbids
// SDK members used as values, so there is no reflective startup capability scan.
function capabilityFailure(message) {
  if (capabilityError !== null) return true;
  const path = 'session\\.(?:id|cwd|version|surfaces|append)|command\\.(?:register|list|run)|process\\.(?:run|spawn)|env\\.get';
  const text = String(message ?? '');
  const missing = new RegExp('\\$\\.(' + path + ')(?:\\(\\.\\.\\.\\))? is not a function\\b').exec(text)
    ?? new RegExp("undefined is not an object \\(evaluating '\\$\\.(" + path + ")'\\)").exec(text);
  if (missing === null) return false;
  capabilityError = 'AgentChat native capability unavailable: ' + missing[1];
  fail({ failure: 'native_capability_unavailable', missing_api: missing[1] }); return true;
}
function eligible(id) { return !faulted && ready && id === current && !paused && !approvals.has(id); }
function activeTurns(id) { return [...turns].filter(([key, root]) => root === id && turnEpoch.get(key) === rootEpoch).map(([key]) => key); }
function exactTurn(id, turn) {
  const ids = activeTurns(id);
  return turn === null ? ids.length === 0 : ids.length === 1 && ids[0] === turn;
}
function scope(root = nativeRoot, captured = generation) { return { root, generation: captured, version: VERSION }; }
function lifecycleAllowed(result) {
  return result !== null && typeof result === 'object' && !Array.isArray(result) && result.block === undefined && result.preventContinuation === undefined;
}
function releaseEarlyStart($) {
  const receipt = earlyStart;
  if (receipt === null || !receipt.complete || receipt.activation === null || !ready) return;
  earlyStart = null;
  if (receipt.allowed && !receipt.signal.aborted && receipt.root === current && receipt.activation.revision === rootEpoch && receipt.activation.epoch === epoch && interactive && !faulted && current === nativeRoot && !approvals.has(current) && activeTurns(current).length === 0 && ![...calls].some(c => c.root === current) && pending === null && finalPermit === null) {
    paused = false; bump(); hint($);
  }
}
async function finishResume($) {
  const r = resuming;
  if (r === null || !r.complete || faulted || current !== null || generation !== r.generation || rootEpoch !== r.revision) return false;
  const id = await $.session.id();
  if (resuming !== r || faulted || id !== r.root || current !== null || generation !== r.generation || rootEpoch !== r.revision) return false;
  const permitted = r.allowed && !r.signal.aborted && epoch === r.epoch;
  resuming = null; ended = null; current = id;
  recordPause('root_changed'); paused = true; ready = false; pending = null; finalPermit = null; bump();
  const revision = bumpRoot(), fence = epoch;
  await activate($, id, revision);
  const observed = await $.session.id();
  if (permitted && !r.signal.aborted && ready && !faulted && current === id && nativeRoot === id && rootEpoch === revision && epoch === fence && observed === id && interactive && !approvals.has(id) && activeTurns(id).length === 0 && ![...calls].some(c => c.root === id) && pending === null && finalPermit === null) { paused = false; bump(); hint($); }
  return ready;
}
async function invoke($, command, input) {
  const argv = [AGENTCHAT, 'internal', command, '--native-executable', NATIVE];
  if (command === 'claude-namespace') argv.push('--native-version', VERSION);
  let result;
  try { result = await $.process.run(argv, { stdin: input === undefined ? undefined : JSON.stringify(input), timeoutMs: 10000, env: ENV }); }
  catch (error) { capabilityFailure(error.message); throw error; }
  if (result.exitCode !== 0) { const error = Error('Native integration unavailable'); error.code = result.exitCode; throw error; }
  return JSON.parse(result.stdout);
}
function control($, action, input) { return invoke($, 'claude-control', { action, input }); }
async function facts($) {
  const root = await $.session.id(), cwd = await $.session.cwd(), version = await $.session.version();
  if (NATIVE === '@managed') VERSION = (location ?? await namespace($)).profile.version;
  if (version.version !== VERSION) throw Error('Unsupported native engine');
  return { root, cwd, version: version.version };
}
async function marker($) { return (await $.command.list()).some(c => c.name === MARKER && c.description === DESCRIPTION); }
async function namespace($) {
  const next = await invoke($, 'claude-namespace');
  if (location !== null && (next.directory !== location.directory || next.owner_root !== location.owner_root || next.observer !== location.observer || next.profile.version !== location.profile.version || next.profile.executable !== location.profile.executable)) throw Error('Native owner changed');
  location = next; return next;
}
function activate($, root, revision = rootEpoch) {
  if (root !== current || revision !== rootEpoch) return Promise.resolve(false);
  const key = { root, revision }; desired = key; ready = false;
  if (worker !== null) return worker;
  const work = (async () => {
    while (desired !== null && !faulted) {
      const wanted = desired; desired = null;
      for (let attempt = 0; attempt < 3; attempt++) {
        const actual = await facts($), physical = await namespace($);
        if (current !== wanted.root || actual.root !== wanted.root || wanted.revision !== rootEpoch || desired !== null) break;
        if (!await marker($) || (await $.session.id()) !== wanted.root || wanted.revision !== rootEpoch || desired !== null) break;
        try {
          const ticket = await control($, 'reserve', { ...actual, after_generation: physical.generation_floor });
          if (wanted.revision !== rootEpoch || current !== wanted.root || desired !== null) {
            await control($, 'retire', scope(ticket.reservation.root, ticket.reservation.generation)); break;
          }
          nativeRoot = wanted.root; generation = ticket.reservation.generation; binding = null;
          const registered = await control($, 'register', { reservation: ticket.reservation, version: actual.version });
          if (wanted.revision !== rootEpoch || current !== wanted.root || desired !== null || (await $.session.id()) !== wanted.root || !await marker($)) break;
          binding = registered.registration.registration; ready = true; releaseEarlyStart($); break;
        } catch (error) {
          ready = false;
          if (error.code !== 4 && error.code !== 7) throw error;
          // A retry recaptures root/marker/epoch and a new floor. Transport loss
          // does not clear native pause or permanently fault an otherwise live mod.
        }
      }
    }
    return ready;
  })();
  worker = work;
  void work.finally(() => { if (worker === work) worker = null; }).catch(() => {});
  return work;
}
async function ensure($, id) {
  if (worker !== null) await worker;
  // Ordinary native activity can retry an exhausted bounded stream supervisor.
  // Starting I/O grants neither root authority nor permission to release pause.
  if (id === current) startStream($);
  if (!ready || nativeRoot !== id) return activate($, id);
  const actual = await facts($), captured = generation;
  try {
    const result = await control($, 'check', { ...actual, generation: captured });
    if (actual.root !== id || current !== id) return false;
    return result.needed ? activate($, id) : true;
  } catch (error) { ready = false; if (error.code === 4 || error.code === 7) return activate($, id); throw error; }
}
function hint($) {
  if (!ready || current === null || current !== nativeRoot) return;
  phasePending = true;
  if (phaseBusy) return;
  phaseBusy = true;
  void (async () => {
    while (phasePending) { phasePending = false; if (!ready || current !== nativeRoot) break; await control($, 'refresh', scope()); }
  })().catch(() => {}).finally(() => { phaseBusy = false; });
}
function startStream($) {
  if (streamTask !== null || faulted) return;
  const task = stream($); streamTask = task;
  void task.catch(error => { capabilityFailure(error.message); ready = false; }).finally(() => { if (streamTask === task) streamTask = null; });
}
function reconnect($, nonce) {
  if (faulted || nonce === reconnectSeen) return;
  reconnectSeen = nonce; reconnectWanted = nonce;
  ready = false; pending = null; finalPermit = null; bump();
  if (reconnectTask !== null) return;
  // Use the host's own configured-server command and policy checks. One queued
  // task leaves observation responsive while a turn/approval delays the command.
  // Reconnecting the transport never releases native pause or joins a root.
  const work = (async () => {
    while (reconnectWanted !== null && !faulted) {
      reconnectWanted = null;
      try { await $.command.run({ command: 'mcp', args: 'reconnect agentchat' }); } catch (error) { capabilityFailure(error.message); }
    }
  })();
  reconnectTask = work;
  void work.finally(() => { if (reconnectTask === work) reconnectTask = null; }).catch(() => {});
}
async function stream($) {
  let restarting = false, immediateFailures = 0;
  while (!faulted && await marker($)) {
    await namespace($);
    if (faulted || !await marker($)) return;
    let buffer = '', started = Date.now();
    const argv = [AGENTCHAT, 'internal', 'claude-stream', '--native-executable', NATIVE, '--state-directory', ENV.AGENTCHAT_HOME];
    if (restarting) argv.push('--restart-backoff');
    try { for await (const piece of $.process.spawn({ argv })) {
      if (piece.stream !== 'stdout') continue;
      buffer += piece.text;
      if (buffer.length > 16384) throw Error('Native stream bound');
      while (buffer.includes('\n')) {
        const end = buffer.indexOf('\n'), line = buffer.slice(0, end); buffer = buffer.slice(end + 1);
        const query = JSON.parse(line);
        if (query.event === 'transport_changed') { if (current !== null) await ensure($, current); continue; }
        if (query.event === 'context_refresh') {
          const id = current, context = binding?.stage === 'project' ? binding.registration.context : null;
          // This separate supervisor hint supplies no root or control facts.
          // Recapture SDK identity/marker/epoch through the ordinary registrar;
          // a stale/cross-root hint cannot clear pause or reuse its old scope.
          if (!faulted && id !== null && id === nativeRoot && context?.context_id === query.context_id && generation === query.native_generation && await $.session.id() === id && await marker($)) await activate($, id);
          continue;
        }
        if (query.event === 'transport_lost') {
          if (!/^[a-f0-9-]{36}$/.test(query.endpoint_nonce)) throw Error('Invalid transport loss');
          reconnect($, query.endpoint_nonce); continue;
        }
        if (!/^[a-f0-9-]{36}$/.test(query.nonce) || typeof query.reply_socket !== 'string' || !query.reply_socket.startsWith('/tmp/agentchat-observer-') || query.reply_socket.length >= 100) throw Error('Invalid native question');
        await inputs.flush(inputIo($));
        const captured = generation, root = current, fence = epoch;
        let order = { sequence: 0, current: false };
        try { order = await control($, 'observation', scope(root, captured)); } catch {}
        const id = await $.session.id(), cwd = await $.session.cwd(), surfaces = await $.session.surfaces(), version = await $.session.version(), present = await marker($);
        const active = activeTurns(id);
        const boundCwd = binding?.stage === 'project' ? binding.registration.context.working_directory : binding?.registration?.working_directory;
        const available = cwd === boundCwd && order.current && ready && present && !faulted && id === root && id === current && id === nativeRoot && captured === generation && fence === epoch;
        const answer = { nonce: query.nonce, relay_sequence: query.relay_sequence, native_sequence: order.sequence, observer: location.observer, owner_root: location.owner_root, root: id, generation: captured, epoch: fence, available, paused, pause_cause: pauseCause, approval: approvals.has(id), active_turns: active, guard_present: present, surfaces, version: version.version, unix: Date.now() };
        // A cancelled observation client does not invalidate the attachment.
        await $.process.run([AGENTCHAT, 'internal', 'claude-answer', '--socket', query.reply_socket], { stdin: JSON.stringify(answer), timeoutMs: 5000, env: ENV });
      }
    } } catch (error) { if (capabilityFailure(error.message)) return; }
    ready = false; pending = null; finalPermit = null; bump();
    // Child restarts are paced by the helper. Stop after repeated immediate
    // SDK failures before spawn; a later native lifecycle can retry safely.
    immediateFailures = Date.now() - started < 200 ? immediateFailures + 1 : 0;
    if (immediateFailures >= 3) return;
    restarting = true;
  }
}
async function fault($, e, next) {
  if (capabilityFailure(next.error?.message)) return next.called ? undefined : next(e);
  // Fixed classifications only: never publish arbitrary engine errors or
  // prompt/tool data. Diagnostics grant neither readiness nor root authority.
  const mismatch = /^(Error: )?Native lifecycle root disagreement(?:\n|$)/.test(next.error?.message ?? '');
  const details = { failure: mismatch ? 'lifecycle_root_disagreement' : 'unclassified',
    failure_kind: ['throw', 'timeout'].includes(next.error?.kind) ? next.error.kind : 'unknown' };
  const uuid = /^[a-f0-9-]{36}$/i;
  if (mismatch && typeof e.session_id === 'string' && uuid.test(e.session_id)) details.event_root = e.session_id;
  // Latch first: even a delayed diagnostic getter must never leave delivery
  // eligible after failure. A replaced cause cannot receive this old detail.
  fail(details); const recorded = pauseCause;
  if (mismatch) {
    try { const id = await $.session.id(); if (pauseCause === recorded && uuid.test(id)) recorded.observed_root = id; } catch (error) { capabilityFailure(error.message); }
  }
  return next.called ? undefined : next(e);
}
async function startupGate($, root) {
  if (startupReleased) return true;
  if (startupWaiters >= 32) return false;
  startupWaiters++;
  try {
    const selected = await $.env.get('AGENTCHAT_LAUNCH_ROOT'), launchGeneration = await $.env.get('AGENTCHAT_LAUNCH_GENERATION');
    if (selected == null && launchGeneration == null) { startupReleased = true; return true; }
    const actual = await facts($);
    if (actual.root !== root) return false;
    const captured = { root: actual.root, generation, revision: rootEpoch };
    if (startupWork !== null && (startupWork.root !== captured.root || startupWork.generation !== captured.generation || startupWork.revision !== captured.revision)) return false;
    // Concurrent first inputs share one bounded native helper. No extra input
    // worker/queue is created, and a switched root cannot inherit an old wait.
    if (startupWork === null) {
      const work = { ...captured, promise: invoke($, 'claude-startup', actual) };
      startupWork = work;
      void work.promise.finally(() => { if (startupWork === work) startupWork = null; }).catch(() => {});
    }
    const result = await startupWork.promise;
    if (captured.generation !== generation || captured.revision !== rootEpoch || await $.session.id() !== captured.root) return false;
    if (result.ready === true && result.cacheable === true) startupReleased = true;
    return result.ready === true;
  } finally { startupWaiters--; }
}
export function register(on) {
  on('command.run', { command: MARKER }, async () => ({ text: 'AGENTCHAT_GUARD_PRESENT' }));
  on('command.run', async ($, e, next) => {
    const result = await next(e); await finishResume($); return result;
  }).catch(($, e, next) => { if (!capabilityFailure(next.error?.message) && resuming !== null) { paused = true; ready = false; } return next.called ? undefined : next(e); });
  on('session.start', async ($, e, next) => {
    const id = await $.session.id();
    current = id; recordPause('session_start'); paused = true; bump(); const revision = bumpRoot();
    if (earlyStart?.root !== id) earlyStart = null;
    if (earlyStart !== null) earlyStart.activation = { revision, epoch };
    await $.command.register({ name: MARKER, description: DESCRIPTION });
    started = true;
    interactive = e.isInteractive === true && e.surface === 'terminal';
    // Observation/reconnect I/O starts even if initial registration races MCP.
    startStream($);
    await activate($, id, revision);
    releaseEarlyStart($);
    return next(e);
  }).catch(fault);
  on('classic.SessionStart', async ($, e, next) => {
    const id = await $.session.id();
    if (id !== e.session_id) {
      // Native 302 resumes announce their destination before the SDK switches.
      // Retain only a selector after the exact old root ended. No binding or
      // permission is granted until a later native boundary agrees with it.
      if (e.source !== 'resume' || faulted || current !== null || ended?.reason !== 'resume' || ended.root !== id || nativeRoot !== id || ended.generation !== generation || ended.revision !== rootEpoch || ended.epoch !== epoch) throw Error('Native lifecycle root disagreement');
      const receipt = { ...ended, root: e.session_id, signal: next.signal, complete: false, allowed: false };
      resuming = receipt;
      const result = await next(e);
      receipt.allowed = next.origin?.plugin === 'engine' && next.origin?.tier === 'core' && lifecycleAllowed(result);
      receipt.complete = true; return result;
    }
    ended = null; resuming = null;
    const permittedSource = e.source === 'resume' || (e.source === 'startup' && initialStart && (current === null || current === id));
    initialStart = false;
    if (!started) {
      // Classic startup can precede the mounted SDK session. Retain only one
      // exact downstream receipt; command/observer APIs wait for session.start.
      // A later activation must still match root, epoch, core origin and signal.
      const activation = current === id ? { revision: rootEpoch, epoch } : null;
      const receipt = { root: id, signal: next.signal, activation, complete: false, allowed: false };
      earlyStart = receipt;
      const result = await next(e);
      receipt.allowed = permittedSource && next.origin?.plugin === 'engine' && next.origin?.tier === 'core' && lifecycleAllowed(result);
      receipt.complete = true; releaseEarlyStart($);
      return result;
    }
    earlyStart = null;
    if (current !== id) { current = id; recordPause('root_changed'); paused = true; pending = null; finalPermit = null; bump(); }
    bumpRoot();
    startStream($);
    await activate($, id);
    const captured = generation, fence = epoch, result = await next(e), observed = await $.session.id();
    const allowed = lifecycleAllowed(result);
    if (allowed && permittedSource && next.origin?.plugin === 'engine' && next.origin?.tier === 'core' && interactive && !next.signal.aborted && !faulted && current === id && observed === id && nativeRoot === id && generation === captured && epoch === fence && !approvals.has(id) && activeTurns(id).length === 0 && ![...calls].some(c => c.root === id) && pending === null && finalPermit === null) { paused = false; bump(); hint($); }
    return result;
  }).catch(fault);
  on('session.end', async ($, e, next) => {
    if (earlyStart?.root === e.sessionId) earlyStart = null;
    if (pending?.root === e.sessionId) pending = null;
    if (current === e.sessionId) {
      current = null; recordPause('session_end'); paused = true; ready = false; finalPermit = null; bumpRoot(); bump();
      ended = { root: e.sessionId, reason: e.reason, generation, revision: rootEpoch, epoch };
    }
    return next(e);
  }).catch(fault);
  on('tool.call', async ($, e, next) => {
    await finishResume($);
    const id = await $.session.id(), revision = rootEpoch, call = { root: id }; let captured = null, stopped = false;
    if (calls.size >= 32) { fail(); return next(e); }
    calls.add(call);
    const aborted = () => { if (!stopped && current === id && rootEpoch === revision) { stopped = true; recordPause('tool_cancelled', { tool: e.tool, call_id: e.tool_use_id, agent: e.agentId }, next.signal); paused = true; pending = null; finalPermit = null; bump(); hint($); } };
    next.signal.addEventListener('abort', aborted, { once: true });
    try {
      if (next.signal.aborted) aborted();
      if (e.tool.startsWith('mcp__agentchat__') && e.agentId === undefined) {
        if (!await ensure($, id) || id !== current || id !== nativeRoot) throw Error('Native root is unavailable');
        captured = scope();
        await control($, 'record', { scope: captured, tool_name: e.tool, tool_use_id: e.tool_use_id });
      }
      return await next(e);
    } finally {
      if (next.signal.aborted) aborted(); next.signal.removeEventListener('abort', aborted); calls.delete(call);
      if (approvals.has(id) && ![...calls].some(c => c.root === id)) { approvals.delete(id); bump(); hint($); }
      if (captured !== null) {
        try { await control($, 'revoke', { scope: captured, tool_use_id: e.tool_use_id }); } catch {}
        if (!next.signal.aborted && !faulted && current === id && nativeRoot === id) await ensure($, id);
      }
    }
  }).catch(fault);
  on('classic.PermissionRequest', async ($, e, next) => {
    const id = await $.session.id();
    if (id !== current || id !== e.session_id || ![...calls].some(c => c.root === id)) fail();
    approvals.add(id); pending = null; finalPermit = null; bump(); hint($); return next(e);
  }).catch(fault);
  on('turn.start', async ($, e, next) => {
    if (e.agentId !== undefined) return next(e);
    if (current === null || turns.size >= 32) fail(); else { turns.set(e.turnId, current); turnEpoch.set(e.turnId, rootEpoch); }
    hint($); return next(e);
  }).catch(fault);
  on('turn.complete', async ($, e, next) => {
    if (e.agentId !== undefined) return next(e);
    const id = turns.get(e.turnId), revision = turnEpoch.get(e.turnId);
    if (id === undefined) fail(); else if ((e.isAborted || e.reason === 'error') && current === id && revision === rootEpoch) { recordPause(e.isAborted ? 'turn_aborted' : 'turn_error', { turn_id: e.turnId, reason: e.reason }); paused = true; pending = null; finalPermit = null; bump(); }
    turns.delete(e.turnId); turnEpoch.delete(e.turnId); hint($); return next(e);
  }).catch(fault);
  on('prompt.submit', async ($, e, next) => {
    if (capabilityError !== null) return { drop: capabilityError };
    await finishResume($);
    if (next.signal.aborted || !await startupGate($, await $.session.id()) || next.signal.aborted) return { drop: 'AgentChat startup is not complete; retry after initialization' };
    if (inputs.owns(e)) return inputs.prompt(inputIo($), e, next);
    const observed = current, revision = rootEpoch; let stopped = false;
    const stop = () => {
      if (!stopped && observed !== null && current === observed && rootEpoch === revision) {
        stopped = true; recordPause(next.signal.aborted ? 'prompt_cancelled' : 'prompt_dropped', { origin: e.origin?.kind }, next.signal);
        paused = true; bump(); hint($);
      }
    };
    next.signal.addEventListener('abort', stop, { once: true });
    try {
      if (next.signal.aborted) stop();
      if (e.origin?.kind === 'composer' && e.isMeta !== true && await $.session.id() === current) { paused = false; bump(); hint($); }
      const result = await next(e); if (typeof result?.drop === 'string') stop(); return result;
    } finally { next.signal.removeEventListener('abort', stop); }
  }).catch(async ($, e, next) => { if (!capabilityFailure(next.error?.message)) fail(); return capabilityError !== null ? { drop: capabilityError } : (!startupReleased || inputs.owns(e) ? { drop: 'AgentChat guard unavailable' } : (next.called ? undefined : next(e))); });
  on('classic.UserPromptSubmit', async ($, e, next) => {
    return inputs.final(inputIo($), e, next);
  }).catch(async ($, e, next) => {
    if (!capabilityFailure(next.error?.message)) fail();
    // An owned queued body must not fall through when its final guard fails.
    return capabilityError !== null ? { block: capabilityError } : inputs.ownsFinal(e) ? { block: 'AgentChat input guard unavailable' }
      : (next.called ? undefined : next(e));
  });
  on('classic.FileChanged', ($, e, next) => inputs.changed(inputIo($), e, next)).catch(fault);
  on('classic.Stop', ($, e, next) => inputs.changed(inputIo($), e, next, true)).catch(fault);
  on('session.append', ($, e, next) => inputs.row(inputIo($), e, next)).catch(async ($, e, next) => {
    if (next.called && inputs.uncertainRow(e)) return next(e); // SDK catch replay never re-enters core.
    const owned = inputs.ownsRow(e); fail();
    // A guard fault before queue entry cannot fall through with the full body.
    return next.called ? undefined : (owned ? inputs.rejectRow(e, next) : next(e));
  });
  on('classic.SessionEnd', async ($, e, next) => {
    if (e.session_id === nativeRoot) { try { await control($, 'retire', scope()); } catch {} }
    return next(e);
  });
}
