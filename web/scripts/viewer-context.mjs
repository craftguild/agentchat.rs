/** Development-only metadata; this root file is never copied into dist. */
import { readFile, realpath, writeFile } from 'node:fs/promises';
import { basename, join } from 'node:path';

/** Preserve user HMR sessions by writing only changed development metadata. */
export async function writeDevContext(web) {
  const directory = await realpath(join(web, '..'));
  const path = join(web, 'viewer-context.json'), text = JSON.stringify({directoryName:basename(directory) || 'Root'}) + '\n';
  if (await readFile(path, 'utf8').catch(() => '') !== text) await writeFile(path, text);
}
