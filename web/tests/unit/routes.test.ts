import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseRoute, routePath, plainClick } from '../../src/routes.ts';

test('channel and thread URLs roundtrip without inventing a default', () => {
  const id = '00000000-0000-0000-0000-000000000083';
  assert.deepEqual(parseRoute('/'), {channel:'',root:null});
  for (const channel of ['dev','general','lab05-rmi','graphql']) {
    assert.equal(routePath(channel),`/${channel}`);
    assert.deepEqual(parseRoute(routePath(channel)),{channel,root:null});
    assert.deepEqual(parseRoute(routePath(channel,id)),{channel,root:id});
  }
});
test('malformed and ambiguous Viewer paths do not select another channel', () => {
  for (const path of ['/dev/','/dev/bad','/a%2Fb','/%','/../dev','/c/dev',
    '/a--b','/a-','/'+ 'a'.repeat(65),'/dev/00000000-0000-0000-0000-000000000083/extra']) {
    assert.equal(parseRoute(path),null,path);
  }
});
test('only an unmodified primary click is an in-document navigation', () => {
  const click={button:0,metaKey:false,ctrlKey:false,shiftKey:false,altKey:false};
  assert.equal(plainClick(click as MouseEvent),true);
  for (const modified of [{button:1},{metaKey:true},{ctrlKey:true},{shiftKey:true},{altKey:true}]) {
    assert.equal(plainClick({...click,...modified} as MouseEvent),false);
  }
});
