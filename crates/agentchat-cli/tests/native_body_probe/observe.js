// Manual real-host instrumentation. This is not the product FIFO or admission.
const ROOT = __ROOT__, PYTHON = __PYTHON__, EXPECTED = __EXPECTED__;
let root = null, paused = true, approval = false, epoch = 0;
async function log($, kind, data) {
  await $.process.run([PYTHON, ROOT + '/hook.py', ROOT, 'log'], {
    stdin: JSON.stringify({ kind, data }), timeoutMs: 5000,
  });
}
async function facts($) {
  return { root: await $.session.id(), cwd: await $.session.cwd(), version: await $.session.version() };
}
function own(text) { return typeof text === 'string' && text.includes('BODY_PROBE_NOTICE'); }
function stop() { paused = true; epoch++; }
export function register(on) {
  on('session.append', async ($, e, next) => {
    const relevant = e.message.content.some(block => own(block.text));
    const captured = relevant ? { root: await $.session.id(), door: e.door,
      origin: e.origin, core: next.origin, uuid: e.uuid, type: e.message.type,
      role: e.message.role ?? null } : null;
    const result = await next(e);
    if (captured !== null) await log($, 'native_row_kept', { ...captured,
      result, aborted: next.signal.aborted });
    return result;
  });
  on('session.start', async ($, e, next) => {
    root = await $.session.id(); stop(); approval = false;
    await log($, 'session_start_before', { ...await facts($), event: e });
    const result = await next(e);
    await log($, 'session_start_after', { ...await facts($), result: result ?? null, aborted: next.signal.aborted });
    return result;
  });
  on('classic.SessionStart', async ($, e, next) => {
    await log($, 'classic_start_before', e);
    const result = await next(e);
    await log($, 'classic_start_after', { event: e, result: result ?? null, aborted: next.signal.aborted });
    return result;
  });
  on('classic.FileChanged', async ($, e, next) => {
    const id = await $.session.id(), captured = epoch;
    const allowed = id === EXPECTED && id === root && !paused && !approval;
    await log($, 'file_before', { event: e, root: id, allowed, epoch: captured });
    const result = await next({ ...e, agentchat_body_probe: { root: id, allowed, epoch: captured } });
    await log($, 'file_after', { root: await $.session.id(), result: result ?? null, aborted: next.signal.aborted, epoch });
    return result;
  });
  on('prompt.submit', async ($, e, next) => {
    const id = await $.session.id(), isOwn = own(e.text);
    if (!isOwn && e.origin?.kind === 'composer' && e.isMeta !== true && id === root) {
      paused = false; epoch++;
    }
    await log($, 'prompt_before', { ...await facts($), event: e, epoch, paused, approval });
    if (isOwn && (id !== EXPECTED || id !== root || paused || approval)) {
      await log($, 'prompt_drop', { root: id, epoch, paused, approval });
      return { drop: 'The disposable body probe is not currently eligible' };
    }
    const aborted = () => { if (root === id) stop(); };
    next.signal.addEventListener('abort', aborted, { once: true });
    try {
      const result = await next(e);
      await log($, 'prompt_after', { root: await $.session.id(), event: e, result: result ?? null, aborted: next.signal.aborted, epoch, paused, approval });
      return result;
    } finally { next.signal.removeEventListener('abort', aborted); }
  });
  on('classic.UserPromptSubmit', async ($, e, next) => {
    await log($, 'classic_prompt_before', { root: await $.session.id(), event: e });
    const result = await next(e);
    await log($, 'classic_prompt_after', { root: await $.session.id(), event: e, result: result ?? null, aborted: next.signal.aborted });
    return result;
  });
  on('prompt.attachment', async ($, e, next) => {
    const result = await next(e);
    const relevant = own(e.text) || own(result?.text);
    await log($, 'attachment_after', { root: await $.session.id(),
      event: relevant ? e : { type: e.type, origin: e.origin, textLength: e.text?.length },
      result: relevant ? result ?? null : { textLength: result?.text?.length },
      aborted: next.signal.aborted, paused, approval, epoch });
    return result;
  });
  on('turn.start', async ($, e, next) => {
    await log($, 'turn_start', { root: await $.session.id(), event: e, paused, approval, epoch });
    return next(e);
  });
  on('turn.step', async function* ($, e, next) {
    await log($, 'model_request', { root: await $.session.id(), keys: Object.keys(e), turn: e.turnId, epoch, paused, approval });
    yield* next(e);
  });
  on('turn.complete', async ($, e, next) => {
    if (e.isAborted || e.reason === 'error') stop();
    await log($, 'turn_complete', { root: await $.session.id(), event: e, paused, approval, epoch });
    return next(e);
  });
  on('tool.call', async ($, e, next) => {
    const id = await $.session.id();
    const aborted = () => { if (root === id) stop(); };
    next.signal.addEventListener('abort', aborted, { once: true });
    try {
      return await next(e);
    } finally {
      if (next.signal.aborted) aborted();
      next.signal.removeEventListener('abort', aborted); approval = false;
      await log($, 'tool_after', { root: id, tool: e.tool, call: e.tool_use_id, aborted: next.signal.aborted, paused, epoch });
    }
  });
  on('classic.PermissionRequest', async ($, e, next) => {
    approval = true; epoch++;
    await log($, 'approval_before', { root: await $.session.id(), event: e, epoch });
    return next(e);
  });
  on('session.end', async ($, e, next) => {
    stop();
    await log($, 'session_end', { root, event: e, epoch });
    root = null;
    return next(e);
  });
}
