/** Own only development children, with bounded startup and shutdown. */
import { spawn } from 'node:child_process';

export const children = new Set();

/** Spawn without a shell, keeping an awaitable exit even after startup. */
export function child(command, args, options = {}) {
  const process = spawn(command, args, {stdio:['ignore', 'pipe', 'pipe'], ...options});
  children.add(process);
  process.completion = new Promise((resolve, reject) => {
    process.once('error', reject);
    process.once('close', (code, signal) => resolve({code, signal}));
  });
  // Startup and exit can race with callers attaching their awaiters.
  void process.completion.catch(() => {});
  void process.completion.finally(() => children.delete(process)).catch(() => {});
  return process;
}

/** Run a finite scaffold command; diagnostics remain bounded on failure. */
export async function run(command, args, options = {}) {
  const {signal, ...spawnOptions} = options;
  signal?.throwIfAborted();
  const process = child(command, args, spawnOptions);
  let diagnostic = '';
  for (const stream of [process.stdout, process.stderr]) {
    stream.setEncoding('utf8');
    stream.on('data', chunk => { diagnostic = (diagnostic + chunk).slice(-16_000); });
  }
  let abort;
  const cancelled = new Promise((_, reject) => {
    abort = () => reject(signal.reason);
    signal?.addEventListener('abort', abort, {once:true});
    if (signal?.aborted) abort();
  });
  try {
    const result = await Promise.race([process.completion, cancelled]);
    if (result.code !== 0) throw new Error(`${command} ${args.slice(0, 2).join(' ')} failed: ${diagnostic}`);
  } finally {signal?.removeEventListener('abort', abort);}
}

/** Stop all owned children and reap them, escalating after a short grace period. */
export async function stopChildren() {
  const owned = [...children];
  for (const process of owned) process.kill('SIGTERM');
  const timer = setTimeout(() => {
    for (const process of owned) if (children.has(process)) process.kill('SIGKILL');
  }, 3000);
  try { await Promise.allSettled(owned.map(process => process.completion)); }
  finally { clearTimeout(timer); }
}

/** Chigiri reports the ephemeral listener on stderr; never guess its port. */
export function mockReady(process, signal, timeout = 30_000) {
  return new Promise((resolve, reject) => {
    let pending = '', diagnostic = '';
    const timer = setTimeout(() => finish(new Error(`Chigiri Mock startup timed out:\n${diagnostic}`)), timeout);
    const abort = () => finish(signal.reason);
    const finish = (error, url) => {
      clearTimeout(timer); signal.removeEventListener('abort', abort);
      process.stderr.off('data', data);
      if (error) reject(error); else resolve(url);
    };
    const data = chunk => {
      pending = (pending + chunk).slice(-32_000); diagnostic = (diagnostic + chunk).slice(-16_000);
      let end;
      while ((end = pending.indexOf('\n')) >= 0) {
        const line = pending.slice(0, end); pending = pending.slice(end + 1);
        const match = line.match(/http:\/\/127\.0\.0\.1:(\d+)/);
        if (match) { finish(null, match[0]); return; }
      }
    };
    process.stderr.setEncoding('utf8'); process.stderr.on('data', data);
    process.stdout.resume();
    signal.addEventListener('abort', abort, {once:true});
    if (signal.aborted) abort();
    void process.completion.then(result => finish(new Error(`Chigiri Mock exited (${result.code ?? result.signal}):\n${diagnostic}`)), finish);
  });
}
