import assert from 'node:assert/strict';
import { test } from 'node:test';
import { cp, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { loadNotices, npmPackage } from '../../scripts/notices.mjs';

const web = fileURLToPath(new URL('../../', import.meta.url));

test('distribution retains exact locked package licenses and source copyrights', async () => {
  const notices = await loadNotices();
  assert.equal(notices.names.size, 37);
  assert(notices.names.has('lucide-react'));
  assert(notices.names.has('@apollo/client'));
  assert(!notices.names.has('lit'));
  assert(notices.names.has('react'));
  assert(notices.names.has('react-hook-form'));
  assert(notices.names.has('zod'));
  assert(notices.names.has('jdenticon'));
  assert(notices.names.has('@fontsource/ibm-plex-sans'));
  assert(notices.names.has('@fontsource/ibm-plex-mono'));
  assert.match(notices.text, /SIL OPEN FONT LICENSE/);
  assert.match(notices.text, /Redistribution and use in source and binary forms/);
  assert.match(notices.text, /Copyright \(c\) Microsoft Corporation/);
  assert.equal(npmPackage('/project/web/node_modules/@lit/reactive-element/index.js'), '@lit/reactive-element');
  assert.equal(npmPackage('/project/web/src/app.ts'), undefined);
});

test('dependency and upstream license drift refuse a distribution build', async () => {
  const root = await mkdtemp(join(tmpdir(), 'agentchat-notices-'));
  try {
    await cp(join(web, 'licenses'), join(root, 'licenses'), { recursive:true });
    const originalLock = await readFile(join(web, 'package-lock.json'), 'utf8');
    const entries = JSON.parse(await readFile(join(web, 'licenses/packages.json'), 'utf8'));
    for (const entry of entries) {
      const path = `node_modules/${entry.name}`;
      await cp(join(web, path, 'package.json'), join(root, path, 'package.json'), { recursive:true });
      if (entry.file.startsWith('node_modules/')) await cp(join(web, entry.file), join(root, entry.file));
      if (entry.notice) await cp(join(web, path, 'index.js'), join(root, path, 'index.js'));
    }
    await writeFile(join(root, 'package-lock.json'), originalLock);
    await loadNotices(root);
    const licensePath = join(root, 'node_modules/react/LICENSE');
    const license = await readFile(licensePath);
    await writeFile(licensePath, 'different license');
    await assert.rejects(loadNotices(root), /changed license react/);
    await writeFile(licensePath, license);
    const lock = JSON.parse(originalLock);
    lock.packages['node_modules/react'].version = '0.0.0';
    await writeFile(join(root, 'package-lock.json'), JSON.stringify(lock));
    await assert.rejects(loadNotices(root), /dependency drift react/);
    lock.packages['node_modules/new-package'] = { version:'1.0.0', license:'MIT' };
    await writeFile(join(root, 'package-lock.json'), JSON.stringify(lock));
    await assert.rejects(loadNotices(root), /package inventory drift/);
  } finally { await rm(root, { recursive:true, force:true }); }
});
