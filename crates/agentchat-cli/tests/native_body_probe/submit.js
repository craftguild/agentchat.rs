// Manual queued-input observation; not the product dispatcher/control module.
const ROOT = __ROOT__, PYTHON = __PYTHON__, EXPECTED = __EXPECTED__;
let root = null, paused = true, approval = false, busy = false, accepted = 0;
const turns = new Set();
async function log($, kind, data) {
  const result = await $.process.run([PYTHON, ROOT + '/hook.py', ROOT, 'log'], {
    stdin: JSON.stringify({ kind, data }), timeoutMs: 5000,
  });
  if (result.exitCode !== 0) throw Error('Probe log failed');
}
async function eligible($) {
  if (root !== EXPECTED || await $.session.id() !== root || paused || approval) return false;
  const owner = JSON.parse(await $.fs.read(ROOT + '/owner.json'));
  const manifest = JSON.parse(await $.fs.read(ROOT + '/manifest.json'));
  const current = await $.process.run([manifest.identity_helper, 'identity', String(owner.pid)], { timeoutMs: 3000 });
  return current.exitCode === 0 && JSON.stringify(JSON.parse(current.stdout)) === JSON.stringify(owner);
}
function own(text) { return typeof text === 'string' && text.includes('AGENTCHAT_SUBMIT_PROBE root=' + EXPECTED); }
export function register(on) {
  on('session.start', async ($, e, next) => {
    root = await $.session.id(); paused = true; approval = false;
    await log($, 'submit_session', { root, version: await $.session.version(), event: e });
    return next(e);
  });
  on('classic.SessionStart', async ($, e, next) => {
    await log($, 'submit_classic_session', e); return next(e);
  });
  on('prompt.submit', async ($, e, next) => {
    const isOwn = own(e.text), id = await $.session.id();
    if (!isOwn && e.origin?.kind === 'composer' && !e.isMeta && id === root) paused = false;
    await log($, 'submit_prompt', { root: id, event: e, paused, approval, active: [...turns] });
    if (isOwn && !await eligible($)) return { drop: 'Probe root or controls changed' };
    return next(e);
  });
  on('session.append', async ($, e, next) => {
    const relevant = e.message.content.some(block => own(block.text));
    const result = await next(e);
    if (relevant) await log($, 'submit_native_row', {
      root: await $.session.id(), door: e.door, origin: e.origin, core: next.origin,
      uuid: e.uuid, result,
    });
    return result;
  });
  on('classic.FileChanged', async ($, e, next) => {
    await log($, 'submit_file', { root: await $.session.id(), paused, approval, busy, accepted, active: [...turns], event: e });
    if (!busy) {
      busy = true;
      try {
        const control = JSON.parse(await $.fs.read(ROOT + '/control.json'));
        while (accepted < (control.through ?? 0) && await eligible($)) {
          const sequence = accepted + 1;
          const payload = JSON.parse(await $.fs.read(ROOT + '/payloads/' + String(sequence).padStart(20, '0') + '.json'));
          if (payload.root !== EXPECTED || payload.sequence !== sequence) throw Error('Wrong payload');
          const text = 'AGENTCHAT_SUBMIT_PROBE root=' + EXPECTED + '\n' + JSON.stringify(payload);
          await log($, 'submit_before', { root, sequence, textLength: text.length, active: [...turns] });
          const result = await $.prompt.submit({ text });
          const exact = result.drop === undefined && result.text === text;
          await log($, 'submit_result', { root, sequence, exact, result, active: [...turns] });
          if (!exact) break;
          // Fixture selection only; no AGENTCHAT delivery cursor or read/ACK is changed.
          accepted = sequence;
        }
      } finally { busy = false; }
    }
    return next({ ...e, agentchat_body_probe: { root, allowed: false } });
  });
  on('turn.start', async ($, e, next) => {
    if (e.agentId === undefined) turns.add(e.turnId);
    await log($, 'submit_turn', { root: await $.session.id(), event: e }); return next(e);
  });
  on('turn.step', async function* ($, e, next) {
    await log($, 'submit_model_request', { root: await $.session.id(), turn: e.turnId }); yield* next(e);
  });
  on('turn.complete', async ($, e, next) => {
    if (e.isAborted || e.reason === 'error') paused = true;
    turns.delete(e.turnId);
    await log($, 'submit_complete', { root: await $.session.id(), event: e, paused }); return next(e);
  });
  on('tool.call', async ($, e, next) => {
    const stopped = () => { paused = true; };
    next.signal.addEventListener('abort', stopped, { once: true });
    await log($, 'submit_tool_before', { root: await $.session.id(), tool: e.tool, call: e.tool_use_id });
    try { return await next(e); }
    finally {
      if (next.signal.aborted) stopped();
      next.signal.removeEventListener('abort', stopped); approval = false;
      // Abort may cancel this process-based logger. The native pause latch above
      // is still set; the next independent FileChanged observation reports it.
      try { await log($, 'submit_tool_after', { tool: e.tool, aborted: next.signal.aborted, paused }); } catch {}
    }
  });
  on('classic.PermissionRequest', async ($, e, next) => {
    approval = true;
    await log($, 'submit_approval', { root: await $.session.id(), event: e }); return next(e);
  });
  on('session.end', async ($, e, next) => {
    paused = true; root = null;
    await log($, 'submit_end', e); return next(e);
  });
}
