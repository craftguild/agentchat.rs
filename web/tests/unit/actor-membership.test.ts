import assert from 'node:assert/strict';
import { test } from 'node:test';
import { actors, agents, message } from '../../src/validate.ts';
import { readFileSync } from 'node:fs';

const id = '00000000-0000-0000-0000-000000000004';
const page = {items:[{id,kind:'USER',handle:'user',joined:true,host:null,chatId:null}],nextCursor:null,scanCursor:'5',head:'5'};

test('anonymous members retain typed identity without fabricated native host fields', () => {
  assert.equal(actors(page).items[0]!.kind,'USER');
  assert.equal(actors(page).items[0]!.host,null);
  assert.throws(() => agents(page));
  for (const extra of [{host:'codex'}, {chatId:'borrowed-native-root'}, {kind:'unknown'}, {kind:undefined}]) {
    assert.throws(() => actors({...page,items:[{...page.items[0],...extra}]}));
  }
  const native = {...page.items[0],kind:'AGENT',host:'codex',chatId:'opaque:chat'};
  assert.equal(actors({...page,items:[native]}).items[0]!.chatId,'opaque:chat');
  assert.throws(() => actors({...page,items:[{...native,chatId:null}]}));
});

test('message authors distinguish the shared user from an agent with the same handle', () => {
  const fixture = JSON.parse(readFileSync(new URL('../../fixtures/contract-v1.json',import.meta.url),'utf8'));
  const row = fixture.roots.items[0];
  assert.equal(message({...row,author:{id,kind:'USER',handle:'user'}}).author.kind,'USER');
  assert.equal(message({...row,author:{id,kind:'AGENT',handle:'user'}}).author.kind,'AGENT');
  assert.throws(() => message({...row,author:{id,handle:'user'}}));
});
