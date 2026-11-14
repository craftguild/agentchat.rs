import type { ChannelView } from './types.ts';

export const ARCHIVE_MILLISECONDS = 30 * 24 * 60 * 60 * 1_000;

/** Reclassify one derived projection without changing identity or selection. */
export function classifyChannel(channel: ChannelView, serverNow: number): ChannelView {
  const archived = Date.parse(channel.archiveAt) <= serverNow;
  return archived === channel.archived ? channel : { ...channel, archived };
}

/** Apply a newer committed root/reply time, ignoring replayed older changes. */
export function recordConversation(channel: ChannelView, createdAt: string, serverNow: number): ChannelView {
  const created = Date.parse(createdAt);
  if (created <= Date.parse(channel.lastConversationAt)) return classifyChannel(channel, serverNow);
  return { ...channel, lastConversationAt:createdAt,
    archiveAt:new Date(created + ARCHIVE_MILLISECONDS).toISOString(), archived:created + ARCHIVE_MILLISECONDS <= serverNow };
}
