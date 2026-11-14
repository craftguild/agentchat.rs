import { compare, WireError } from './types.ts';
import type { Agent, AgentPage, Author } from './types.ts';

/** Drain bounded identity pages and retain joined and departed identities for presentation. */
export async function retainedAgents(
  read: (after:string | null, signal:AbortSignal) => Promise<AgentPage>, signal:AbortSignal,
): Promise<Agent[]> {
  return retainedActors(read,signal);
}

/** Shared bounded directory drain; user membership never needs native host fields. */
export async function retainedActors<T extends Author & {joined:boolean}>(
  read:(after:string | null, signal:AbortSignal) => Promise<{items:T[];nextCursor:string | null;head:string}>, signal:AbortSignal,
):Promise<T[]> {
  const agents = new Map<string, T>();
  let after:string | null = null, head:string | null = null;
  for (;;) {
    const page = await read(after, signal); signal.throwIfAborted();
    head ??= page.head;
    if (compare(page.head, head) < 0) throw new WireError('Agent history changed. Reload the snapshot.', 'agent_reset', true);
    for (const agent of page.items) agents.set(agent.id, agent);
    // First-join keys are internal to shared reads. A fixed initial head bounds
    // continuation; later committed changes trigger another complete refresh.
    if (!page.nextCursor || compare(page.nextCursor, head) >= 0) break;
    if (compare(page.nextCursor, after ?? '0') <= 0) throw new WireError('Agent paging made no progress.', 'agent_gap', true);
    after = page.nextCursor;
  }
  return [...agents.values()].sort((a,b) => a.handle.localeCompare(b.handle));
}

/** Joined sidebar subset of the retained presentation directory. */
export async function participatingAgents(
  read: (after:string | null, signal:AbortSignal) => Promise<AgentPage>, signal:AbortSignal,
): Promise<Agent[]> {
  return (await retainedAgents(read, signal)).filter(agent => agent.joined);
}
