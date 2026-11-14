/** Adapt actual Vite/Rolldown inclusion evidence to the pinned npm adapter. */
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const web = fileURLToPath(new URL('..', import.meta.url));
const root = resolve(web, '..');

export function bundleEvidence() {
  return {
    name: 'agentchat-bundle-evidence',
    apply: 'build',
    async writeBundle(_options, bundle) {
      const inputs = {}, outputs = {}, helpers = new Set();
      for (const [name, item] of Object.entries(bundle)) {
        const contributions = {};
        if (item.type === 'chunk') {
          for (const [id, module] of Object.entries(item.modules)) {
            if (!module.renderedLength) continue;
            const path = id.split('?')[0];
            if (!path.startsWith(web)) {
              if (id.startsWith('\0') || id.startsWith('vite/')) {
                helpers.add(name);
                continue;
              }
              throw new Error('Unowned bundled module: ' + id);
            }
            const source = await readFile(path);
            const key = relative(web, path).replaceAll('\\', '/');
            inputs[key] = { bytes: source.length };
            // Rolldown reports post-tree-shaking module inclusion before final
            // chunk minification. Keep that raw fact; do not claim byte offsets.
            contributions[key] = { bytesInOutput: Buffer.byteLength(module.code ?? '') || module.renderedLength };
          }
        }
        const source = await readFile(resolve(web, 'dist', name));
        outputs['dist/' + name] = { bytes: source.length, inputs: contributions };
      }
      await mkdir(resolve(root, '.craftguild'), { recursive: true });
      await writeFile(resolve(root, '.craftguild/viewer-metafile.json'),
        JSON.stringify({ inputs, outputs, agentchatViteHelpers: [...helpers] }, null, 2) + '\n');
    },
  };
}

/** Add outputs emitted after Vite and precise non-module asset ownership. */
export async function finishBundleEvidence(assets) {
  const path = resolve(root, '.craftguild/viewer-metafile.json');
  const meta = JSON.parse(await readFile(path, 'utf8'));
  const supplemental = [{
    name: 'FIGlet Small Slant banner', version: '2.1-1996-layout', type: 'file',
    reference: 'urn:agentchat:small-slant-banner', license: 'BSD-3-Clause',
    files: ['crates/agentchat-cli/assets/banner.txt', 'crates/agentchat-cli/assets/FIGlet-LICENSE',
      'crates/agentchat-cli/assets/NOTICE.md'],
    evidence: 'The committed ASCII artwork is emitted by the fixed Standards renderer; font attribution and distribution terms are retained. The font/renderer are build-tool inputs.',
  }];
  for (const asset of assets) {
    meta.outputs['dist/' + asset.path] ??= { bytes: asset.bytes, inputs: {} };
    if (meta.outputs['dist/' + asset.path].bytes !== asset.bytes) {
      throw new Error('Bundle output changed after Vite: ' + asset.path);
    }
  }
  const actual = assets.map(asset => 'dist/' + asset.path).sort();
  if (JSON.stringify(Object.keys(meta.outputs).sort()) !== JSON.stringify(actual)) {
    throw new Error('Bundle output inventory differs from embedded assets');
  }
  const lock = JSON.parse(await readFile(resolve(web, 'package-lock.json'), 'utf8'));
  const fonts = assets.filter(asset => asset.path.endsWith('.woff2'));
  for (const font of fonts) {
    const family = font.path.includes('ibm-plex-mono') ? 'ibm-plex-mono' : 'ibm-plex-sans';
    const name = '@fontsource/' + family, entry = lock.packages['node_modules/' + name];
    if (!entry) throw new Error('Unknown embedded font: ' + font.path);
    supplemental.push({ name, version: entry.version, type: 'file',
      reference: 'urn:agentchat:font:' + font.path, license: entry.license,
      files: ['web/dist/' + font.path],
      evidence: 'Vite emitted this locked Fontsource IBM Plex font; the exact bytes and upstream OFL notice are retained by the embedded asset and license inventories.' });
  }
  if (meta.agentchatViteHelpers.length) {
    supplemental.push({ name: 'vite-runtime-helpers', version: lock.packages['node_modules/vite'].version,
      type: 'library', reference: 'urn:agentchat:vite-runtime-helpers', license: 'MIT',
      files: meta.agentchatViteHelpers.map(name => 'web/dist/' + name),
      evidence: 'Vite virtual preload/runtime modules appear in these actual Rolldown output chunks; Vite attribution is included in the shipped notices.' });
  }
  delete meta.agentchatViteHelpers;
  await writeFile(path, JSON.stringify(meta, null, 2) + '\n');
  await writeFile(resolve(root, '.craftguild/viewer-supplemental.json'),
    JSON.stringify(supplemental, null, 2) + '\n');
}
