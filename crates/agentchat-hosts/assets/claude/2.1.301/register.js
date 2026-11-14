// Version-coupled native controls. Physical locators never identify a root.
const AGENTCHAT = __AGENTCHAT_EXECUTABLE__, NATIVE = __AGENTCHAT_NATIVE_EXECUTABLE__, ENV = __AGENTCHAT_ENVIRONMENT__;
const INSTANCE = Date.now().toString(36) + '-' + Math.random().toString(36).slice(2, 10);
const MARKER = 'agentchat-native-guard', DESCRIPTION = 'AgentChat native guard ' + INSTANCE;
let location = null, nativeRoot = null, generation = 0, binding = null, ready = false;
let current = null, paused = true, faulted = false, epoch = 0, serial = 0;
let pending = null, finalPermit = null, desired = null, worker = null, streamTask = null;
let rootEpoch = 0;
let reconnectTask = null, reconnectWanted = null, reconnectSeen = null;
let interactive = false, phasePending = false, phaseBusy = false;
let started = false, earlyStart = null;
let pauseCause = null;
const turns = new Map(), turnEpoch = new Map(), calls = new Set(), approvals = new Set();
function recordPause(code, details = {}, signal) {
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
  if (!Number.isSafeInteger(epoch + 1)) { recordPause('epoch_exhausted'); faulted = true; paused = true; return; }
  epoch++;
}
function bumpRoot() { if (!Number.isSafeInteger(rootEpoch + 1)) { fail(); return rootEpoch; } return ++rootEpoch; }
function fail() { recordPause('guard_fault'); faulted = true; paused = true; ready = false; pending = null; finalPermit = null; earlyStart = null; bump(); }
function eligible(id) { return !faulted && ready && id === current && !paused && !approvals.has(id); }
function activeTurns(id) { return [...turns].filter(([key, root]) => root === id && turnEpoch.get(key) === rootEpoch).map(([key]) => key); }
function exactTurn(id, turn) {
  const ids = activeTurns(id);
  return turn === null ? ids.length === 0 : ids.length === 1 && ids[0] === turn;
}
function scope(root = nativeRoot, captured = generation) { return { root, generation: captured, version: '2.1.301' }; }
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
function notice(text) {
  if (typeof text !== 'string' || text.length > 32768) return null;
  const found = [...text.matchAll(/AGENTCHAT_TOKEN_NOTICE root=([a-f0-9-]{36}) generation=(\d{1,16}) token=([a-z0-9-]{1,100})/g)];
  if (found.length !== 1 || !Number.isSafeInteger(Number(found[0][2]))) return null;
  return { root: found[0][1], generation: Number(found[0][2]), token: found[0][3] };
}
async function invoke($, command, input) {
  const result = await $.process.run([AGENTCHAT, 'internal', command, '--native-executable', NATIVE], {
    stdin: input === undefined ? undefined : JSON.stringify(input), timeoutMs: 10000, env: ENV
  });
  if (result.exitCode !== 0) { const error = Error('Native integration unavailable'); error.code = result.exitCode; throw error; }
  return JSON.parse(result.stdout);
}
function control($, action, input) { return invoke($, 'claude-control', { action, input }); }
async function facts($) {
  const root = await $.session.id(), cwd = await $.session.cwd(), version = await $.session.version();
  if (version.version !== '2.1.301') throw Error('Unsupported native engine');
  return { root, cwd, version: version.version };
}
async function marker($) { return (await $.command.list()).some(c => c.name === MARKER && c.description === DESCRIPTION); }
async function namespace($) {
  const next = await invoke($, 'claude-namespace');
  if (location !== null && (next.directory !== location.directory || next.owner_root !== location.owner_root || next.observer !== location.observer)) throw Error('Native owner changed');
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
  void task.catch(() => { ready = false; }).finally(() => { if (streamTask === task) streamTask = null; });
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
      try { await $.command.run({ command: 'mcp', args: 'reconnect agentchat' }); } catch {}
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
        if (query.event === 'transport_lost') {
          if (!/^[a-f0-9-]{36}$/.test(query.endpoint_nonce)) throw Error('Invalid transport loss');
          reconnect($, query.endpoint_nonce); continue;
        }
        if (!/^[a-f0-9-]{36}$/.test(query.nonce) || typeof query.reply_socket !== 'string' || !query.reply_socket.startsWith('/tmp/agentchat-observer-') || query.reply_socket.length >= 100) throw Error('Invalid native question');
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
    } } catch {}
    ready = false; pending = null; finalPermit = null; bump();
    // Child restarts are paced by the helper. Stop after repeated immediate
    // SDK failures before spawn; a later native lifecycle can retry safely.
    immediateFailures = Date.now() - started < 200 ? immediateFailures + 1 : 0;
    if (immediateFailures >= 3) return;
    restarting = true;
  }
}
async function acceptedActive($, p, id) {
  if (p === null || p.expected_turn_id === null || !exactTurn(id, p.expected_turn_id)) return false;
  try {
    const receipt = JSON.parse(await $.fs.read(location.directory + '/submitted-' + p.context_id + '.json'));
    const delay = receipt.unix_ms - p.issued;
    return receipt.accepted === true && receipt.token === p.dispatch && receipt.context_id === p.context_id && receipt.incarnation === p.incarnation && receipt.root === p.root && receipt.generation === p.generation && Number.isFinite(delay) && delay >= 0 && delay < 2000;
  } catch { return false; }
}
function retirePermits(root, turn) {
  if (root !== current || root !== nativeRoot) return;
  if (pending?.root === root && pending.expected_turn_id === turn) { pending = null; bump(); }
  if (finalPermit?.root === root && finalPermit.expected_turn_id === turn) { finalPermit = null; bump(); }
}
async function fault($, e, next) { fail(); return next.called ? undefined : next(e); }
export function register(on) {
  on('command.run', { command: MARKER }, async () => ({ text: 'AGENTCHAT_GUARD_PRESENT' }));
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
    if (id !== e.session_id) throw Error('Native lifecycle root disagreement');
    if (!started) {
      // Classic startup can precede the mounted SDK session. Retain only one
      // exact downstream receipt; command/observer APIs wait for session.start.
      // A later activation must still match root, epoch, core origin and signal.
      const activation = current === id ? { revision: rootEpoch, epoch } : null;
      const receipt = { root: id, signal: next.signal, activation, complete: false, allowed: false };
      earlyStart = receipt;
      const result = await next(e);
      receipt.allowed = e.source === 'resume' && next.origin?.plugin === 'engine' && next.origin?.tier === 'core' && lifecycleAllowed(result);
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
    if (allowed && e.source === 'resume' && next.origin?.plugin === 'engine' && next.origin?.tier === 'core' && interactive && !next.signal.aborted && !faulted && current === id && observed === id && nativeRoot === id && generation === captured && epoch === fence && !approvals.has(id) && activeTurns(id).length === 0 && ![...calls].some(c => c.root === id) && pending === null && finalPermit === null) { paused = false; bump(); hint($); }
    return result;
  }).catch(fault);
  on('session.end', async ($, e, next) => {
    if (earlyStart?.root === e.sessionId) earlyStart = null;
    if (pending?.root === e.sessionId) pending = null;
    if (current === e.sessionId) { current = null; recordPause('session_end'); paused = true; ready = false; finalPermit = null; bumpRoot(); bump(); }
    return next(e);
  }).catch(fault);
  on('tool.call', async ($, e, next) => {
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
    turns.delete(e.turnId); turnEpoch.delete(e.turnId); if (revision === rootEpoch) retirePermits(id, e.turnId); hint($); return next(e);
  }).catch(fault);
  on('prompt.submit', async ($, e, next) => {
    const observed = current, revision = rootEpoch; let stopped = false;
    const stop = () => { if (!stopped && observed !== null && current === observed && rootEpoch === revision) { stopped = true; recordPause(next.signal.aborted ? 'prompt_cancelled' : 'prompt_dropped', { origin: e.origin?.kind, own: e.text.includes('AGENTCHAT_TOKEN_NOTICE'), meta: e.isMeta === true }, next.signal); paused = true; pending = null; finalPermit = null; bump(); hint($); } };
    next.signal.addEventListener('abort', stop, { once: true });
    try {
      if (next.signal.aborted) stop();
      const id = await $.session.id(), own = e.text.includes('AGENTCHAT_TOKEN_NOTICE');
      if (e.origin?.kind === 'composer' && e.isMeta !== true && !own && id === current) { paused = false; bump(); hint($); }
      if (!own) { const result = await next(e); if (typeof result?.drop === 'string') stop(); return result; }
      const parsed = notice(e.text), p = pending, fence = epoch, age = p === null ? -1 : Date.now() - p.issued;
      const validAge = age >= 0 && (age < 15000 || await acceptedActive($, p, id));
      const allowed = eligible(id) && e.origin?.kind === 'task-notification' && p !== null && parsed !== null && exactTurn(id, p.expected_turn_id) && p.root === id && parsed.root === id && parsed.token === p.token && parsed.generation === p.generation && validAge && (await $.session.id()) === id && (await $.session.cwd()) === binding?.registration?.context?.working_directory;
      if (!allowed || pending !== p || epoch !== fence || !eligible(id) || !exactTurn(id, p.expected_turn_id)) return { drop: 'AgentChat notification is no longer eligible' };
      pending = null; finalPermit = { ...p, issued: Date.now(), epoch: fence };
      const result = await next(e); if (typeof result?.drop === 'string') stop(); return result;
    } finally { next.signal.removeEventListener('abort', stop); }
  }).catch(async ($, e, next) => { fail(); return e.text.includes('AGENTCHAT_TOKEN_NOTICE') ? { drop: 'AgentChat guard unavailable' } : (next.called ? undefined : next(e)); });
  on('classic.UserPromptSubmit', async ($, e, next) => {
    if (!e.prompt.includes('AGENTCHAT_TOKEN_NOTICE')) return next(e);
    const id = await $.session.id(), parsed = notice(e.prompt), p = finalPermit, fence = epoch;
    const cwd = await $.session.cwd();
    const allowed = eligible(id) && cwd === binding?.registration?.context?.working_directory && id === e.session_id && p !== null && parsed !== null && p.epoch === fence && exactTurn(id, p.expected_turn_id) && p.root === id && parsed.root === id && parsed.token === p.token && parsed.generation === p.generation && Date.now() - p.issued >= 0 && Date.now() - p.issued < 15000;
    finalPermit = null;
    if (!allowed || epoch !== fence || !eligible(id)) return next(e);
    return next({ ...e, agentchat_guard_final_permit: { ...p, issued: Date.now(), phase: 'final' } });
  }).catch(fault);
  on('classic.FileChanged', async ($, e, next) => {
    if (!ready || binding?.stage !== 'project') return next(e);
    const registered = binding.registration, context = registered.context;
    if (e.file_path !== context.working_directory + '/.agentchat-wakeup') return next(e);
    const id = await $.session.id(); let wake;
    if (id !== current || (await $.session.cwd()) !== context.working_directory) return next(e);
    try { wake = JSON.parse(await $.fs.read(location.directory + '/wake-' + context.context_id + '.json')); } catch { return next(e); }
    if (registered.root !== id || registered.generation !== generation || wake.root !== id || wake.context_id !== context.context_id || wake.incarnation !== context.incarnation || wake.hint.context_id !== context.context_id || wake.hint.incarnation !== context.incarnation || wake.hint.project_id !== context.project_id) return next(e);
    try { const receipt = JSON.parse(await $.fs.read(location.directory + '/submitted-' + context.context_id + '.json')); if (receipt.accepted === true && receipt.token === wake.token && receipt.context_id === wake.context_id && receipt.incarnation === wake.incarnation) return next(e); } catch {}
    if (pending && (Date.now() < pending.issued || Date.now() - pending.issued >= 15000)) { pending = null; bump(); }
    const signalled = Number((await $.fs.read(e.file_path)).trim()), count = wake.generation, fence = epoch;
    if (!Number.isSafeInteger(signalled) || !Number.isSafeInteger(count) || count <= 0 || count > signalled || !eligible(id) || id !== e.session_id || !exactTurn(id, wake.expected_turn_id ?? null) || pending !== null || !Number.isSafeInteger(serial + 1)) return next(e);
    const p = { root: id, generation: count, native_generation: generation, token: INSTANCE + '-' + (++serial).toString(36), issued: Date.now(), context_id: context.context_id, incarnation: context.incarnation, dispatch: wake.token, expected_turn_id: wake.expected_turn_id ?? null };
    if (epoch !== fence || !eligible(id) || pending !== null || !exactTurn(id, p.expected_turn_id)) return next(e);
    pending = p; return next({ ...e, agentchat_guard_permit: p });
  }).catch(fault);
  on('classic.SessionEnd', async ($, e, next) => {
    if (e.session_id === nativeRoot) { try { await control($, 'retire', scope()); } catch {} }
    return next(e);
  });
}
