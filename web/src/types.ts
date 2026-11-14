/** Public DTOs derive from the catalog; semantic guards refine discriminated changes. */
import type { CheckpointFieldsFragment, ChannelFieldsFragment, ChannelViewFieldsFragment,
  MessageFieldsFragment, AgentFieldsFragment, ActorFieldsFragment, AgentsQuery,
  MembersQuery, ChannelsQuery, RootsQuery, PinnedQuery, ProjectQuery } from './graphql/catalog.generated.ts';
export type Checkpoint=CheckpointFieldsFragment;
export type Channel=ChannelFieldsFragment;
export type ChannelView=ChannelViewFieldsFragment;
export type Message=MessageFieldsFragment;
export type Author=Message['author'];
export type ActorKind=Author['kind'];
export type Agent=AgentFieldsFragment & {kind:'AGENT'};
export type Actor=ActorFieldsFragment;
export type AgentPage=Omit<AgentsQuery['agents'],'items'> & {items:Agent[]};
export type ActorPage=MembersQuery['channelMembers'];
export type ChannelPage=ChannelsQuery['channels'];
export type MessageWindow=RootsQuery['roots'];
export type PinnedRoot=PinnedQuery['pinnedRoot'];
export type Project=ProjectQuery['project'];
export type Change = {
  eventId: string; seq: string;
} & ({ kind: 'CHANNEL_CREATED'; channel: Channel; message: null }
  | { kind: 'MESSAGE_CREATED'; channel: null; message: Message });
export interface ChangeBatch {
  after: Checkpoint; scan: Checkpoint; head: Checkpoint; items: Change[]; hasMore: boolean;
}

export class WireError extends Error {
  readonly code: string;
  readonly reset: boolean;
  readonly retry: boolean;
  constructor(message: string, code: string, reset = false, retry = false) {
    super(message); this.code = code; this.reset = reset; this.retry = retry;
  }
}

/** All ordering compares decimal strings losslessly, including above 2^53. */
export function compare(a: string, b: string): number {
  const left = BigInt(a), right = BigInt(b);
  return left < right ? -1 : left > right ? 1 : 0;
}
export function same(a: Checkpoint, b: Checkpoint): boolean {
  return a.projectId === b.projectId && a.seq === b.seq && a.eventId === b.eventId;
}
