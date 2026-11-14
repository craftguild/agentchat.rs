/** Preserve exact upstream notices in the single-binary viewer distribution. */
import { createHash } from 'node:crypto';
import { lstat, readFile, realpath } from 'node:fs/promises';
import { dirname, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const web = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const fail = message => { throw new Error(`Viewer third-party notices: ${message}`); };

/** Refuse dependency/version/notice drift before emitting any distributed text. */
export async function loadNotices(root = web) {
  root = await realpath(root);
  const read = async name => {
    const path = resolve(root, name);
    if (!path.startsWith(root + sep) || !(await lstat(path)).isFile()
        || await realpath(path) !== path) fail(`nonregular/escaped ${name}`);
    return readFile(path);
  };
  const entries = JSON.parse(await read('licenses/packages.json'));
  const lock = JSON.parse(await read('package-lock.json'));
  const names = entries.map(entry => entry.name);
  if (names.length !== new Set(names).size) fail('duplicate package');
  // Include all production dependencies, even type-only and tree-shaken ones.
  // Vite's injected preload helper and Tailwind's generated CSS retain notices too.
  const expected = Object.entries(lock.packages).filter(([path, value]) => path
    && (!value.dev || path === 'node_modules/vite' || path === 'node_modules/tailwindcss'))
    .map(([path]) => path.replace(/^node_modules\//, '')).sort();
  if (JSON.stringify([...names].sort()) !== JSON.stringify(expected)) fail('package inventory drift');
  const parts = ['AgentChat viewer third-party notices',
    'Includes all locked production npm packages and the Vite/Tailwind build notices.'];
  for (const entry of entries) {
    const locked = lock.packages[`node_modules/${entry.name}`];
    const installed = JSON.parse(await read(`node_modules/${entry.name}/package.json`));
    if (locked.version !== entry.version || installed.version !== entry.version
        || installed.name !== entry.name || installed.license !== entry.license
        || locked.license !== entry.license || locked.integrity !== entry.integrity) {
      fail(`dependency drift ${entry.name}`);
    }
    const bytes = await read(entry.file);
    if (digest(bytes) !== entry.sha256) fail(`changed license ${entry.name}`);
    if (entry.notice) {
      const source = (await read(`node_modules/${entry.name}/index.js`)).toString('utf8');
      if (!source.startsWith(entry.notice)) fail(`changed source notice ${entry.name}`);
    }
    parts.push(`\n${entry.name}@${entry.version} (${entry.license})\nSource: ${entry.source}\n`,
      entry.notice ?? '', bytes.toString('utf8'));
  }
  return { names: new Set(names), text: parts.join('\n') + '\n' };
}

/** Identify ordinary locked npm modules in Vite's resolved module inventory. */
export function npmPackage(path) {
  const name = path.split('/node_modules/').at(-1);
  return name === path ? undefined : name.split('/').slice(0, name.startsWith('@') ? 2 : 1).join('/');
}
