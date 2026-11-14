/** Freeze product version and source revision before building/serving browser assets. */
import { execFileSync } from 'node:child_process';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/** Read only the product workspace version, independently of frontend tooling. */
export function workspaceVersion(cargo) {
  const section = cargo.split('[workspace.package]')[1]?.split(/^\[/m)[0];
  const version = section?.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  if (!version || !/^\d+\.\d+\.\d+(?:[-+][\w.+-]+)?$/.test(version)) throw new Error('Missing product workspace version.');
  return version;
}

/** Git is build-time only. Never borrow a containing repository's revision. */
export async function writeBuildInfo(root) {
  root = realpathSync(root);
  const version = workspaceVersion(await readFile(join(root, 'Cargo.toml'), 'utf8'));
  let revision = 'unknown';
  const git = args => execFileSync('git', args, {cwd:root,encoding:'utf8',stdio:['ignore','pipe','ignore']}).trim();
  try {
    if (realpathSync(git(['rev-parse','--show-toplevel'])) === root) {
      const head = git(['rev-parse','--verify','HEAD']);
      if (/^(?:[0-9a-f]{40}|[0-9a-f]{64})$/.test(head)) revision = head;
    }
  } catch { /* Source archives truthfully have no Git revision. */ }
  const directory = join(root, 'web/public');
  await mkdir(directory, {recursive:true});
  const path = join(directory, 'build-info.txt'), text = JSON.stringify({version,revision}) + '\n';
  if (await readFile(path, 'utf8').catch(() => '') !== text) await writeFile(path, text);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await writeBuildInfo(resolve(dirname(fileURLToPath(import.meta.url)), '../..'));
}
