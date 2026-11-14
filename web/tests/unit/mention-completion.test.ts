import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mentionToken, mentionOptions } from '../../src/mention-completion.ts';

test('completion replaces only the token at the cursor, including its remaining suffix', () => {
  assert.deepEqual(mentionToken('Hello @bl-otter, world',9),{start:6,end:15,query:'bl'});
  assert.deepEqual(mentionToken('@',1),{start:0,end:1,query:''});
  for (const value of ['email@bob','word@bob','/path/@bob','hello @bob ']) assert.equal(mentionToken(value,value.length),null);
  assert.deepEqual(mentionOptions('B',['blue-otter']),[]);
  assert.deepEqual(mentionOptions('bl',['blue-otter','green-wren']),['blue-otter']);
  assert.deepEqual(mentionOptions('',[]),['all','here','channel']);
});
