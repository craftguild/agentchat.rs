/** Couple temporary Mock authoring and serving to one executable and its types. */
import { createHash } from 'node:crypto';
import { createReadStream, constants } from 'node:fs';
import { access, chmod, copyFile, realpath, rm } from 'node:fs/promises';
import { delimiter, join, resolve } from 'node:path';

async function digest(path) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest('hex');
}

/** Resolve PATH without a shell or an implicit binary download/build. */
async function executable(binary) {
  const candidates = binary.includes('/') ? [resolve(binary)]
    : (process.env.PATH ?? '').split(delimiter).map(directory => resolve(directory, binary));
  for (const candidate of candidates) {
    try {await access(candidate, constants.X_OK); return await realpath(candidate);}
    catch (error) {if (binary.includes('/')) throw error;}
  }
  throw new Error(`Chigiri CLI is unavailable: ${binary}`);
}

/** A concurrent rebuild must not change the executable between init and serve. */
export async function snapshotCli(binary, directory) {
  const source = await executable(binary), path = join(directory, 'chigiri');
  for (let attempt = 0; attempt < 3; attempt++) {
    const before = await digest(source);
    await copyFile(source, path, constants.COPYFILE_EXCL);
    await chmod(path, 0o700);
    const sha256 = await digest(path);
    if (sha256 === before) return {path, source, sha256};
    await rm(path);
  }
  throw new Error(`Chigiri CLI changed during startup: ${source}. Retry after its build finishes.`);
}

/** The viewer has one PubSub result type; derive its public selector from the
 * generated contract instead of assuming model-scoped or field-scoped naming. */
export function pubsubSelector(declarations) {
  const names = action => new Set([...declarations.matchAll(new RegExp(
    `\\["${action}"\\]\\s*:\\s*\\{\\s*\\["([A-Za-z_][A-Za-z0-9_]*)"\\]\\s*:\\s*\\(`, 'g'))]
    .map(match => match[1]));
  const subscribe = names('subscribe'), publish = names('publish');
  const [selector] = subscribe;
  if (subscribe.size !== 1 || publish.size !== 1 || !publish.has(selector)) {
    throw new Error('Unsupported generated Mock PubSub contract: expected one matching subscribe/publish selector for the viewer feed.');
  }
  return JSON.stringify(selector);
}
