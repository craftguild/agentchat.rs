/** One command owns temporary Chigiri Mock, Vite reload and coordinated cleanup. */
import { mkdtemp, realpath, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';
import { child, mockReady, stopChildren } from './dev-process.mjs';
import { prepareMock } from './dev-project.mjs';
import { snapshotCli } from './dev-compat.mjs';
import { writeBuildInfo } from './build-info.mjs';
import { writeDevContext } from './viewer-context.mjs';
import { generateCatalog } from './graphql.mjs';

const web = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const controller = new AbortController();
const port = Number(process.env.AGENTCHAT_DEV_PORT ?? 5173);
let directory, vite, exitCode = 0;
let signalReceived;
let stopped;
const stopRequested = new Promise(resolve => { stopped = resolve; });
function stop(signal) {
  signalReceived = signal; controller.abort(new Error(`Development server stopped: ${signal}`)); stopped();
}
process.once('SIGINT', stop); process.once('SIGTERM', stop);

try {
  await generateCatalog();
  await writeBuildInfo(resolve(web, '..'));
  await writeDevContext(web);
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('AGENTCHAT_DEV_PORT must be 1..65535.');
  let binary = process.env.CHIGIRI_BIN || 'chigiri';
  if (binary.includes('/')) binary = resolve(binary);
  directory = await mkdtemp(join(await realpath(tmpdir()), 'agentchat-viewer-dev-'));
  const selected = await snapshotCli(binary, directory);
  binary = selected.path;
  console.log(`Chigiri CLI: ${selected.source}\nSHA-256: ${selected.sha256}`);
  const origin = `http://127.0.0.1:${port}`;
  console.log('Preparing isolated Chigiri Mock from graphql/schema.graphql…');
  const project = await prepareMock(binary, directory, web, origin, controller.signal);
  controller.signal.throwIfAborted();
  const mock = child(binary, ['mock', 'serve', '--config-dir', project]);
  const target = await mockReady(mock, controller.signal);
  console.log(`Chigiri Mock: ${target}/graphql (PID ${mock.pid})`);
  // Keep diagnostics flowing without retaining unbounded access logs.
  mock.stderr.pipe(process.stderr);
  vite = await createServer({root:web, server:{host:'127.0.0.1', port, strictPort:true,
    proxy:{'/graphql':{target, changeOrigin:true, timeout:0, proxyTimeout:0}}}});
  controller.signal.throwIfAborted();
  await vite.listen();
  console.log(`\nViewer: ${origin}\nLive fixture: npm run dev:post -- "Hello from Chigiri Mock"\nCtrl-C stops both servers and removes the temporary project.\n`);
  const viteStopped = new Promise((_, reject) => {
    vite.httpServer.once('close', () => reject(new Error('Vite listener stopped unexpectedly.')));
    vite.httpServer.once('error', reject);
  });
  await Promise.race([stopRequested, viteStopped, mock.completion.then(result => {
    if (!signalReceived) throw new Error(`Chigiri Mock stopped unexpectedly (${result.code ?? result.signal}).`);
  })]);
} catch (error) {
  if (!signalReceived) {
    console.error(`Development startup/runtime failed: ${error.message}\nSet CHIGIRI_BIN to a built Chigiri CLI supporting Mock project v9 with matching generated ctx.pubsub.subscribe/publish APIs.`);
    exitCode = 1;
  }
} finally {
  controller.abort();
  // Close proxied SSE by stopping its upstream before draining Vite connections.
  await stopChildren();
  await vite?.close();
  if (directory) await rm(directory, {recursive:true, force:true});
  process.removeListener('SIGINT', stop); process.removeListener('SIGTERM', stop);
  process.exitCode = exitCode;
}
