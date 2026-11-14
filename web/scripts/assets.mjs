/** Seal every owned build input and exact static asset; never copy a workspace. */
import { createHash } from 'node:crypto';
import { lstat, readdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { finishBundleEvidence } from './bundle-evidence.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const directories = ['web/src', 'web/public', 'web/graphql', 'web/fixtures', 'web/scripts', 'web/licenses'];
const inputs = ['Cargo.toml', 'web/package.json', 'web/package-lock.json', 'web/tsconfig.json', 'web/vite.config.ts',
  'web/codegen.ts',
  'web/index.html', 'web/chigiri-provenance.json', 'chigiri.toml',
  'generated/agentchat-viewer-contract/chigiri-generated.json'];
async function walk(path) {
  const info = await lstat(path);
  if (info.isSymbolicLink()) throw new Error(`Symlink in viewer inputs/assets: ${path}`);
  if (info.isFile()) return [path];
  if (!info.isDirectory()) throw new Error(`Nonregular viewer input: ${path}`);
  const entries = await readdir(path);
  const files = await Promise.all(entries.sort().map(name => walk(join(path, name))));
  return files.flat();
}
function hash(bytes) { return createHash('sha256').update(bytes).digest('hex'); }
for (const directory of directories) for (const path of await walk(join(root, directory))) inputs.push(relative(root, path));
const sealed = {};
for (const path of inputs.sort()) sealed[path] = hash(await readFile(join(root, path)));
const assets = [];
let total = 0;
for (const file of await walk(join(root, 'web/dist'))) {
  const path = relative(join(root, 'web/dist'), file);
  if (path === 'agentchat-assets.json') continue;
  if (!/^[a-zA-Z0-9_./-]+$/.test(path)) throw new Error(`Invalid static asset name: ${path}`);
  const bytes = await readFile(file); total += bytes.length;
  if (bytes.length > 1024 * 1024) throw new Error(`Static asset exceeds 1MiB: ${path}`);
  const extension = path.slice(path.lastIndexOf('.'));
  const mime = { '.html':'text/html; charset=utf-8', '.js':'text/javascript; charset=utf-8',
    '.css':'text/css; charset=utf-8', '.svg':'image/svg+xml', '.woff2':'font/woff2',
    '.txt':'text/plain; charset=utf-8' }[extension];
  if (!mime) throw new Error(`Unsupported viewer asset: ${path}`);
  assets.push({ path, mime, bytes:bytes.length, sha256:hash(bytes) });
}
if (!['index.html', 'third-party-notices.txt', 'build-info.txt'].every(name => assets.some(asset => asset.path === name))
    || assets.length > 64 || total > 8 * 1024 * 1024) throw new Error('Invalid embedded asset inventory/budget.');
const manifest = { schemaVersion:1, node:process.version, inputs:sealed, assets };
await writeFile(join(root, 'web/dist/agentchat-assets.json'), JSON.stringify(manifest, null, 2) + '\n');
await finishBundleEvidence([...assets, {
  path: 'agentchat-assets.json',
  bytes: (await readFile(join(root, 'web/dist/agentchat-assets.json'))).length,
}]);
console.log(`Sealed ${assets.length} embedded assets (${total} bytes), ${inputs.length} inputs.`);
