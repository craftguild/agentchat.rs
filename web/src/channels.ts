import type { ChannelPage, ChannelView } from './types.ts';
import { compare, WireError } from './types.ts';
import { classifyChannel } from './archive.ts';

export interface ChannelListing { items: ChannelView[]; asOf: string }

/** Enumerate one finite channel watermark through bounded shared API pages. */
export async function allChannels(
  read: (after: string | null, signal: AbortSignal) => Promise<ChannelPage>,
  signal: AbortSignal,
): Promise<ChannelListing> {
  const channels: ChannelView[] = [];
  let after: string | null = null, head: string | null = null, asOf: string | null = null;
  while (true) {
    const page = await read(after, signal);
    signal.throwIfAborted();
    head ??= page.head; asOf = page.asOf;
    if (compare(page.head, head) < 0) throw new WireError('Channel history changed. Reload the snapshot.', 'channel_reset', true);
    for (const channel of page.items) {
      if (after && compare(channel.creationSeq, after) <= 0) throw new WireError('Channel paging overlapped.', 'channel_gap', true);
      // Later additions arrive through the sole project Subscription. Capturing
      // the first head prevents a busy writer from extending this initial load.
      if (compare(channel.creationSeq, head) > 0) return listing(channels, asOf);
      channels.push(channel);
    }
    if (!page.nextCursor || compare(page.nextCursor, head) >= 0) return listing(channels, asOf);
    if (compare(page.nextCursor, after ?? '0') <= 0) throw new WireError('Channel paging made no progress.', 'channel_gap', true);
    after = page.nextCursor;
  }
}

function listing(items: ChannelView[], asOf: string | null): ChannelListing {
  if (!asOf) throw new WireError('Channel timing metadata is missing.', 'invalid_frame', true);
  const now = Date.parse(asOf);
  return { items:items.map(channel => classifyChannel(channel, now)), asOf };
}
