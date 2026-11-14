import assert from 'node:assert/strict';
import { test } from 'node:test';
import { directoryLabel } from '../../src/viewer-context.ts';

test('viewer directory names are plain text and optional for injected readers', () => {
  for (const directoryName of ['lab05','Root','<chat> & 日本語']) {
    assert.equal(directoryLabel({directoryName}),directoryName);
  }
  assert.equal(directoryLabel({directoryName:null}),null);
  for (const value of [null,{}, {directoryName:''},{directoryName:42}]) {
    assert.throws(() => directoryLabel(value));
  }
});
