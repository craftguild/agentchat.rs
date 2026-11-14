// Manual native-input experiment, not the product FIFO or profile admission.
const ROOT = __ROOT__, PYTHON = __PYTHON__, EXPECTED = __EXPECTED__;
let root = null, paused = true, approval = false, busy = false, accepted = 0;
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
  const current = await $.process.run([manifest.identity_helper, 'identity', String(owner.pid)], {
    timeoutMs: 3000,
  });
  return current.exitCode === 0 && JSON.stringify(JSON.parse(current.stdout)) === JSON.stringify(owner);
}
export function register(on) {
  on('session.start', async ($, e, next) => {
    root = await $.session.id(); paused = true;
    await log($, 'append_session', { root, version: await $.session.version(), event: e });
    return next(e);
  });
  on('classic.SessionStart', async ($, e, next) => {
    await log($, 'append_classic_session', e);
    return next(e);
  });
  on('prompt.submit', async ($, e, next) => {
    if (e.origin?.kind === 'composer' && !e.isMeta && await $.session.id() === root) paused = false;
    await log($, 'append_prompt', { root: await $.session.id(), event: e, paused });
    return next(e);
  });
  on('classic.FileChanged', async ($, e, next) => {
    await log($, 'append_file', { root: await $.session.id(), busy, event: e });
    if (!busy) {
      busy = true;
      try {
        const control = JSON.parse(await $.fs.read(ROOT + '/control.json'));
        while (accepted < (control.through ?? 0) && await eligible($)) {
          const sequence = accepted + 1;
          const payload = JSON.parse(await $.fs.read(ROOT + '/payloads/' + String(sequence).padStart(20, '0') + '.json'));
          if (payload.root !== EXPECTED || payload.sequence !== sequence) throw Error('Wrong immutable payload');
          const text = 'AGENTCHAT_APPEND_PROBE full native input, passive context only.\n' + JSON.stringify(payload);
          await log($, 'append_before', { root, sequence, textLength: text.length });
          const result = await $.session.append({ message: { type: 'user', content: [{ type: 'text', text }] } });
          const exact = !result.deny && result.message?.type === 'user' && result.message?.role === 'user'
            && result.message?.isMeta === true && result.message?.content?.length === 1
            && result.message.content[0].type === 'text' && result.message.content[0].text === text
            && typeof result.uuid === 'string' && result.uuid.length > 0;
          await log($, 'append_result', { root, sequence, exact, result });
          if (!exact) break;
          // Fixture selection only: no AGENTCHAT cursor or acknowledgment is changed.
          accepted = sequence;
        }
      } finally { busy = false; }
    }
    // Observe append alone; the static hook must not perform an async wake.
    return next({ ...e, agentchat_body_probe: { root, allowed: false } });
  });
  on('turn.start', async ($, e, next) => {
    await log($, 'append_turn', { root: await $.session.id(), event: e });
    return next(e);
  });
  on('turn.step', async function* ($, e, next) {
    await log($, 'append_model_request', { root: await $.session.id(), turn: e.turnId });
    yield* next(e);
  });
  on('turn.complete', async ($, e, next) => {
    if (e.isAborted || e.reason === 'error') paused = true;
    await log($, 'append_complete', { root: await $.session.id(), event: e, paused });
    return next(e);
  });
  on('classic.PermissionRequest', async ($, e, next) => {
    approval = true;
    await log($, 'append_approval', { root: await $.session.id(), event: e });
    return next(e);
  });
  on('session.end', async ($, e, next) => {
    paused = true; root = null;
    await log($, 'append_end', e);
    return next(e);
  });
}
