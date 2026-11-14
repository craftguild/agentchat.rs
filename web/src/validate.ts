/** Decode the expected operation's shape before any application progress moves. */
import { compare, WireError } from './types.ts';
import type { ActorKind, ActorPage, AgentPage, Author, Channel, ChannelPage, ChannelView, Change, ChangeBatch, Checkpoint,
  Message, MessageWindow, PinnedRoot, Project } from './types.ts';

function invalid(): never { throw new WireError('Viewer data requires a snapshot reset.', 'invalid_frame', true); }
export function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return invalid();
  return value as Record<string, unknown>;
}
export function text(value: unknown, maximum = 1024): string {
  if (typeof value !== 'string' || value.length > maximum) return invalid();
  return value;
}
function uuid(value: unknown): string {
  const id = text(value, 36);
  if (!/^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(id)) return invalid();
  return id;
}
export function decimal(value: unknown): string {
  const seq = text(value, 19);
  if (!/^(0|[1-9][0-9]*)$/.test(seq) || BigInt(seq) > 9223372036854775807n) return invalid();
  return seq;
}
function count(value: unknown): string {
  const n = text(value, 20);
  if (!/^(0|[1-9][0-9]*)$/.test(n) || BigInt(n) > 18446744073709551615n) return invalid();
  return n;
}
function boolean(value: unknown): boolean { return typeof value === 'boolean' ? value : invalid(); }
function instant(value: unknown): string {
  const encoded = text(value, 64), parsed = Date.parse(encoded);
  if (!Number.isFinite(parsed) || new Date(parsed).toISOString() !== encoded) return invalid();
  return encoded;
}
function list<T>(value: unknown, decode: (item: unknown) => T): T[] {
  if (!Array.isArray(value) || value.length > 200) return invalid();
  return value.map(decode);
}
export function checkpoint(value: unknown): Checkpoint {
  const v = record(value), seq = decimal(v.seq);
  const eventId = v.eventId === null ? null : uuid(v.eventId);
  if ((seq === '0') !== (eventId === null)) return invalid();
  return { projectId: uuid(v.projectId), seq, eventId };
}
export function channel(value: unknown): Channel {
  const v = record(value);
  return { id: uuid(v.id), name: text(v.name, 64), creationSeq: decimal(v.creationSeq) };
}
export function channelView(value: unknown): ChannelView {
  const v = record(value);
  return { ...channel(v), lastConversationAt:instant(v.lastConversationAt), archiveAt:instant(v.archiveAt),
    archived:boolean(v.archived), mandatory:boolean(v.mandatory) };
}
function author(value: unknown): Author {
  const v = record(value);
  const kind:ActorKind = v.kind === 'AGENT' || v.kind === 'USER' ? v.kind : invalid();
  return { id: uuid(v.id), kind, handle: text(v.handle, 64) };
}
export function message(value: unknown): Message {
  const v = record(value);
  return { id: uuid(v.id), channelId: uuid(v.channelId), channel: text(v.channel, 64),
    author: author(v.author), rootId: v.rootId === null ? null : uuid(v.rootId), threadId: uuid(v.threadId),
    creationSeq: decimal(v.creationSeq), createdAt: text(v.createdAt, 64), body: text(v.body, 65_536),
    bodyHtml: text(v.bodyHtml, 512 * 1024), renderFallback: boolean(v.renderFallback), replyCount: count(v.replyCount) };
}
export function window(value: unknown): MessageWindow {
  const v = record(value), snapshot = checkpoint(v.snapshot), items = list(v.items, message);
  if (items.some((m, index) => compare(m.creationSeq, snapshot.seq) > 0
    || (index > 0 && compare(items[index - 1]!.creationSeq, m.creationSeq) >= 0))) return invalid();
  return { snapshot, items, nextBefore: v.nextBefore === null ? null : decimal(v.nextBefore) };
}
export function pinned(value: unknown): PinnedRoot {
  const v = record(value); return { snapshot: checkpoint(v.snapshot), root: message(v.root) };
}
export function channels(value: unknown): ChannelPage {
  const v = record(value); return { items: list(v.items, channelView),
    nextCursor: v.nextCursor === null ? null : decimal(v.nextCursor),
    scanCursor: decimal(v.scanCursor), head: decimal(v.head), asOf:instant(v.asOf) };
}
export function agents(value: unknown): AgentPage {
  const v = record(value);
  return { items:list(v.items, item => { const row = record(item), label = author(row);
    if (label.kind !== 'AGENT') return invalid();
    return { ...label, kind:'AGENT' as const, joined:boolean(row.joined), host:text(row.host, 64) }; }),
    nextCursor:v.nextCursor === null ? null : decimal(v.nextCursor),
    scanCursor:decimal(v.scanCursor), head:decimal(v.head) };
}
export function actors(value: unknown): ActorPage {
  const v = record(value);
  return { items:list(v.items, item => { const row = record(item), label = author(row);
    const host = row.host === null ? null : text(row.host,64), chatId = row.chatId === null ? null : text(row.chatId,1024);
    if (label.kind === 'USER' ? host !== null || chatId !== null : host === null || chatId === null) return invalid();
    return { ...label, joined:boolean(row.joined), host, chatId }; }),
    nextCursor:v.nextCursor === null ? null : decimal(v.nextCursor), scanCursor:decimal(v.scanCursor), head:decimal(v.head) };
}
export function project(value: unknown): Project {
  const v = record(value), head = checkpoint(v.checkpoint), id = uuid(v.id);
  if (head.projectId !== id) return invalid();
  return { id, checkpoint: head };
}
function change(value: unknown): Change {
  const v = record(value), eventId = uuid(v.eventId), seq = decimal(v.seq);
  if (v.kind === 'CHANNEL_CREATED' && v.message === null) {
    const created = channel(v.channel);
    if (created.creationSeq !== seq) return invalid();
    return { eventId, seq, kind: v.kind, channel: created, message: null };
  }
  if (v.kind === 'MESSAGE_CREATED' && v.channel === null) {
    const created = message(v.message);
    if (compare(created.creationSeq, seq) >= 0) return invalid();
    return { eventId, seq, kind: v.kind, channel: null, message: created };
  }
  return invalid();
}
export function batch(value: unknown): ChangeBatch {
  const v = record(value), after = checkpoint(v.after), scan = checkpoint(v.scan), head = checkpoint(v.head);
  const items = list(v.items, change), hasMore = boolean(v.hasMore);
  if (after.projectId !== scan.projectId || after.projectId !== head.projectId
    || compare(after.seq, scan.seq) > 0 || compare(scan.seq, head.seq) > 0
    || hasMore !== (compare(scan.seq, head.seq) < 0)) return invalid();
  let previous = after.seq;
  for (const item of items) {
    if (compare(item.seq, previous) <= 0 || compare(item.seq, scan.seq) > 0) return invalid();
    previous = item.seq;
  }
  return { after, scan, head, items, hasMore };
}
