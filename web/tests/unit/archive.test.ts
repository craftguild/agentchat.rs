import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ARCHIVE_MILLISECONDS, classifyChannel, recordConversation } from '../../src/archive.ts';
import type { ChannelView } from '../../src/types.ts';

const channel = (archiveAt = '2026-10-31T00:00:00.000Z'):ChannelView => ({ id:'1',name:'dev',creationSeq:'1',
  lastConversationAt:'2026-10-01T00:00:00.000Z',archiveAt,archived:false,mandatory:true });

test('the inclusive deadline archives and a newer conversation reactivates', () => {
  assert.equal(classifyChannel(channel(), Date.parse('2026-10-30T23:59:59.999Z')).archived, false);
  assert.equal(classifyChannel(channel(), Date.parse('2026-10-31T00:00:00.000Z')).archived, true);
  const next = recordConversation({...channel(),archived:true}, '2026-10-26T12:00:00.000Z', Date.parse('2026-10-31T00:00:00.000Z'));
  assert.equal(next.archived, false);
  assert.equal(Date.parse(next.archiveAt) - Date.parse(next.lastConversationAt), ARCHIVE_MILLISECONDS);
});
