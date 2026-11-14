// One database-owned head spans passive history and addressed native queueing.
// Native queue entry is delivery completion; later host processing is separate.
// Host access comes through ordinary operation closures from the entry module;
// every literal SDK call stays visible to that module's native static analyzer.
export function createInputs({ AGENTCHAT, NATIVE, ENV, state }) {
  const queued = new Map(), MAX_QUEUED = 32;
  let busy = null, waiting = 0, pending = null, completed = null, flushing = null, processing = null;
  let unknown = null, failedRow = null;
  const key = file => file.context_id + ':' + file.token;
  const notice = file => 'AGENTCHAT_INPUT_NOTICE root=' + file.root + ' context=' + file.context_id + ' token=' + file.token + '\n' + file.text;
  const owns = e => e.origin?.kind === 'task-notification'
    && (e.text?.includes('AGENTCHAT_INPUT_NOTICE root=') || e.text?.includes('AGENTCHAT_INPUT_CANCELLED '));
  async function call(io, mode, input) {
    const result = await io.run([AGENTCHAT, 'internal', 'claude-input', '--mode', mode,
      '--native-executable', NATIVE, '--state-directory', ENV.AGENTCHAT_HOME], {
      stdin: JSON.stringify(input), timeoutMs: 5000, env: ENV,
    });
    if (result.exitCode !== 0) throw Error('Native full input unavailable');
    return JSON.parse(result.stdout);
  }
  async function eligible(io, captured) {
    const now = state();
    return now.eligible && now.root === captured.root && now.root === now.nativeRoot
      && now.generation === captured.generation && now.epoch === captured.epoch
      && now.revision === captured.revision && now.context?.context_id === captured.context?.context_id
      && now.context?.incarnation === captured.context?.incarnation
      && await io.id() === now.root && await io.cwd() === now.context?.working_directory
      && await io.marker();
  }
  async function claim(io,file) {
    const result=await call(io,'claim',{root:file.root,generation:file.native_generation,context_id:file.context_id,token:file.token});
    if (result.claimed !== true) { unresolved(file); return false; }
    return true;
  }
  async function enter(io, value) {
    // Idle native prompts may precede their queue row, or run inside core's
    // row continuation. Both share this exact durable one-shot entry intent.
    try { return await (value.entry ??= claim(io, value.file)); }
    catch (error) { unresolved(value.file); throw error; }
  }
  function refuse(value) {
    if (value.started) unresolved(value.file);
    else remember(value.file, { route: 'rejected_before_entry' });
  }
  function remember(file, native) { completed = { file, native }; }
  function unresolved(file) { unknown = { root: file.root, generation: file.native_generation }; }
  function cancel() {
    if (pending !== null && !pending.receiving) {
      if (completed === null || key(completed.file) !== key(pending.file)) remember(pending.file, { route: 'rejected_before_entry' });
      pending = null;
    }
    // Keep accepted frames through pause: a native internal queue/storage
    // continuation is not another AGENTCHAT submission. Wrong roots remain fenced.
    const now = state();
    if (unknown !== null && (unknown.root !== now.root || unknown.generation !== now.generation)) unknown = null;
    for (const [id,value] of queued) if (value.file.root !== now.root
      || value.file.native_generation !== now.generation) queued.delete(id);
  }
  async function flush(io) {
    if (completed === null) return true;
    if (flushing !== null) return flushing;
    const value = completed;
    const work = (async () => {
      try {
        await call(io, 'record', { root: value.file.root, generation: value.file.native_generation,
          context_id: value.file.context_id, token: value.file.token, native: value.native });
        if (completed === value) completed = null;
        return true;
      } catch { return false; }
    })();
    flushing = work;
    try { return await work; } finally { if (flushing === work) flushing = null; }
  }
  async function changed(io, e, next, boundary = false) {
    if (unknown !== null) return next(e);
    await flush(io);
    let captured = state();
    if (waiting >= 32 || !captured.context || (!boundary && e.file_path !== captured.context.working_directory + '/.agentchat-wakeup')
      || e.session_id !== captured.root || !await eligible(io, captured)) return next(e);
    // Serialize the existing callbacks themselves. Dropping a change while an
    // earlier callback is returning could lose the successor's only idle signal.
    // No detached consumer, timer or second native scheduling path is created.
    waiting++;
    while (busy !== null) await busy;
    let release; busy = new Promise(resolve => { release = resolve; });
    try {
      captured = state();
      if (unknown !== null || e.session_id !== captured.root || !await eligible(io, captured) || next.signal.aborted) return next(e);
      if (boundary) for (const [id,value] of queued) if (value.processingDone) queued.delete(id);
      let loaded;
      try { loaded = await call(io, 'load', { root: captured.root, generation: captured.generation }); } catch { return next(e); }
      const file = loaded.file;
      if (file === null || !await eligible(io, captured) || next.signal.aborted) return next(e);
      if (completed !== null) {
        if (key(completed.file) === key(file)) return next(e);
        completed = null; // A committed successor fenced the old callback.
      }
      if (loaded.addressed) {
        // No overflow worker/timer: a real Stop boundary retries a deferred
        // publication after queue continuation frees one of these 32 slots.
        if (queued.size >= MAX_QUEUED || queued.has(key(file))) return next(e);
        if (pending !== null && key(pending.file) === key(file)) return next(e);
        const permit = { root: file.root, generation: file.native_generation,
          context_id: file.context_id, incarnation: file.incarnation, token: file.token,
          epoch: captured.epoch, issued: null, queue: null };
        pending = { file, proof: loaded.proof, permit, captured, text: notice(file), receiving: false };
        return next({ ...e, agentchat_input_permit: permit });
      }
      let native;
      try {
        if (!await claim(io,file)) return next(e);
        if (!await eligible(io,captured) || next.signal.aborted) { remember(file,{route:'rejected_before_entry'});await flush(io);return next(e); }
        native = { route: 'append', result: await io.append({ message: {
        type: 'user', content: [{ type: 'text', text: file.text }],
      } }) }; } catch {
        // A thrown return does not prove that the host rejected the row. Keep
        // the original application head and block callbacks already queued here.
        unresolved(file); return next(e);
      }
      remember(file, native); await flush(io);
      return next(e);
    } finally { busy = null; waiting--; release(); }
  }
  async function row(io, e, next) {
    if (e.agentId !== undefined || e.door !== 'queue' || next.origin?.plugin !== 'engine'
      || next.origin?.tier !== 'core' || e.origin?.kind !== 'engine') return next(e);
    const value = pending;
    const text = value === null ? null : notice(value.file);
    const blocks = e.message.content.filter(block => block.type === 'text');
    const relevant = value !== null && blocks.some(block => block.text.includes(text));
    const accepted = [...queued.values()].filter(v => blocks.some(block => block.text.includes(v.text)));
    const owned = relevant || accepted.length !== 0 || blocks.some(block => block.text.startsWith('<task-notification>')
      && block.text.includes('AGENTCHAT_INPUT_NOTICE root='));
    if (!owned) return next(e);
    const id = await io.id();
    const known = [...accepted.filter(v => v.file.root === id).map(v => v.text), ...(relevant ? [text] : [])];
    const proved = blocks.every(block => {
      const ranges = known.flatMap(frame => {
        const found = []; let offset = 0;
        while (offset <= block.text.length) {
          const at = block.text.indexOf(frame, offset); if (at < 0) break;
          found.push([at,at+frame.length]); offset = at+frame.length;
        }
        return found;
      });
      return [...block.text.matchAll(/AGENTCHAT_INPUT_NOTICE root=/g)].every(match => ranges.some(([a,b]) => match.index>=a && match.index<b));
    });
    if (!proved) {
      // A static hook may finish after its pending attempt was cancelled or
      // replaced. Strip only the owned native notification, before core entry;
      // a later prompt guard alone would be too late to prevent acceptance.
      return rejectRow(e, next);
    }
    if (!relevant) {
      try { return await next(e); }
      finally { for (const value of accepted) if (value.processingDone) queued.delete(key(value.file)); }
    }
    if (!await eligible(io, value.captured) || next.signal.aborted) {
      // Respect cancellation before native queue entry. A recognizable empty
      // cancellation marker is later dropped, never treated as a full body.
      const content = e.message.content.map(block => block.type === 'text'
        ? { ...block, text: block.text.replace(text, 'AGENTCHAT_INPUT_CANCELLED ' + value.file.token) } : block);
      refuse(value); if (pending === value) pending = null;
      const result = await next({ ...e, message: { ...e.message, content } });
      await flush(io); return result;
    }
    value.receiving = true;
    try {
      if (!await enter(io,value)) return rejectRow(e,next);
      if (!await eligible(io,value.captured) || next.signal.aborted) {
        refuse(value);await flush(io);return rejectRow(e,next);
      }
      value.started = true;
      const result = await next(e);
      // Preserve prompt-first processing flags; a returned row is still the
      // only native result from which an acceptance receipt can be recorded.
      queued.set(key(value.file), Object.assign(value, { id: e.uuid, text }));
      remember(value.file, { route: 'queued', result: { root: id, door: e.door,
        origin: e.origin, core: next.origin, uuid: e.uuid, result } });
      await flush(io); return result;
    } catch (error) {
      // Core may have kept the row before a later continuation lost its result.
      // No negative receipt and no second native submission are justified.
      unresolved(value.file); failedRow = { id: e.uuid, text };
      // A normal hook must return its native result. Let the SDK catch lane
      // relay only its replay-safe original continuation, without a new row or
      // AGENTCHAT receipt. A lost result is not an identity/lifecycle failure.
      throw error;
    } finally { if (pending === value) pending = null; }
  }
  function uncertainRow(e) {
    return failedRow !== null && e.uuid === failedRow.id && e.agentId === undefined
      && e.door === 'queue' && e.origin?.kind === 'engine'
      && e.message.content.some(block => block.type === 'text' && block.text.includes(failedRow.text));
  }
  function ownsRow(e) {
    return e.agentId === undefined && e.door === 'queue' && e.origin?.kind === 'engine'
      && e.message.content.some(block => block.type === 'text' && block.text.startsWith('<task-notification>')
        && block.text.includes('AGENTCHAT_INPUT_NOTICE root='));
  }
  function rejectRow(e, next) {
    return next({ ...e, message: { ...e.message, content: e.message.content.map(block => block.type === 'text'
      && block.text.includes('AGENTCHAT_INPUT_NOTICE root=') ? { ...block, text: 'AGENTCHAT_INPUT_CANCELLED stale native notice' } : block) } });
  }
  function entries(text) {
    const candidates = new Map(queued);
    if (pending !== null && unknown === null) candidates.set(key(pending.file), pending);
    const found = [...candidates.values()].filter(value => text.includes(value.text));
    const ranges = found.map(value => [text.indexOf(value.text), text.indexOf(value.text) + value.text.length]);
    const markers = [...text.matchAll(/AGENTCHAT_INPUT_NOTICE root=/g)];
    return markers.every(match => ranges.some(([start,end]) => match.index >= start && match.index < end)) ? found : [];
  }
  async function prompt(io, e, next) {
    const captured = state(), values = entries(e.text);
    if (!owns(e) || values.length === 0 || values.some(v => v.processingStarted || v.file.root !== captured.root
      || v.file.native_generation !== captured.generation || v.file.context_id !== captured.context?.context_id
      || v.file.incarnation !== captured.context?.incarnation)
      || !await eligible(io, captured) || next.signal.aborted) return { drop: 'AgentChat queued input is no longer eligible' };
    // Recheck after the awaited native observations: concurrent prompts must
    // not both continue the original head while its entry helper is running.
    if (values.some(v => v.processingStarted)) return { drop: 'AgentChat input already has a native continuation' };
    for (const value of values) value.processingStarted = true;
    const active = { values, captured };
    try {
      for (const value of values) if (value.id === undefined) {
        value.receiving = true;
        if (!await enter(io, value)) return { drop: 'AgentChat pending input entry is unavailable' };
      }
      if (!await eligible(io, captured) || next.signal.aborted) return { drop: 'AgentChat input was interrupted before continuation' };
      processing = active;
      for (const value of values) value.started = true;
      return await next(e);
    }
    finally {
      if (processing === active) processing = null;
      for (const value of values) value.processingDone = true;
    }
  }
  async function final(io, e, next) {
    if (!ownsFinal(e)) return next(e);
    const active = processing;
    if (active === null || active.values.some(value => !e.prompt.includes(value.text)) || !await eligible(io, active.captured)) {
      // Function-hook ClassicResult differs from a command hook's JSON shape.
      // A legacy decision/reason result is skipped by the native engine.
      return { block: 'AgentChat queued input lost its original native permit' };
    }
    // A pending head carries its actual frame proof, never an invented native
    // row UUID. Rust checks that original current head and its one-shot intent.
    // Accepted rows instead survive AGENTCHAT head retirement through their queue UUID.
    const permits = active.values.map(value => ({ ...value.permit, epoch: state().epoch,
      issued: Date.now(), queue: value.id === undefined ? null : { id: value.id, ...value.proof },
      head: value.id === undefined ? value.proof : null }));
    return next({ ...e, agentchat_input_final_permits: permits });
  }
  function ownsFinal(e) {
    return typeof e.prompt === 'string' && e.prompt.startsWith('<task-notification>')
      && e.prompt.includes('AGENTCHAT_INPUT_NOTICE root=');
  }
  return { owns, ownsRow, uncertainRow, ownsFinal, rejectRow, changed, row, prompt, final, cancel, flush };
}
