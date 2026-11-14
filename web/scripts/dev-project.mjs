/** Build an isolated, resettable Chigiri Mock project from the product SDL. */
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { run } from './dev-process.mjs';
import { common, window, changes, post } from './dev-resolvers.mjs';
import { createChannel, postMessage, replyMessage } from './dev-user-actions.mjs';
import { pubsubSelector } from './dev-compat.mjs';
import { receiptFixtures, messageDetail } from './dev-receipts.mjs';

const id = value => `00000000-0000-0000-0000-${value.toString(16).padStart(12, '0')}`;

/** Small ordered fixtures exercise channels, older pages, threads and Markdown. */
export function fixtures() {
  const channels = ['dev', 'design', 'random'].map((name, index) => ({id:id(10 + index), name, creationSeq:String(index + 1)}));
  const events = channels.map(channel => ({eventId:id(20000 + Number(channel.creationSeq)), seq:channel.creationSeq,
    kind:'CHANNEL_CREATED', channel, message:null}));
  const messages = [];
  function add(channel, root = null) {
    const seq = String(Number(events.at(-1).seq) + 1), auditSeq = String(Number(seq) + 1), messageId = id(10000 + Number(seq));
    const message = {id:messageId, channelId:channel.id, channel:channel.name,
      author:{kind:'AGENT',id:id(3 + messages.length % 3), handle:['violet-fox', 'blue-otter', 'green-wren'][messages.length % 3]},
      rootId:root?.id ?? null, threadId:root?.id ?? messageId, creationSeq:seq,
      createdAt:new Date(Date.UTC(2026, 9, 24, 9, messages.length)).toISOString(),
      body:`${root ? 'Thread reply' : 'Development message'} ${seq}\n\nHello **AgentChat** — 日本語の表示確認。`,
      bodyHtml:`<p>${root ? 'Thread reply' : 'Development message'} ${seq}</p><p>Hello <strong>AgentChat</strong> — 日本語の表示確認。</p>`,
      renderFallback:false, replyCount:'0'};
    messages.push(message);
    events.push({eventId:id(20000 + Number(auditSeq)), seq:auditSeq, kind:'MESSAGE_CREATED', channel:null, message});
    return message;
  }
  for (const channel of channels) {
    for (let index = 0; index < (channel.name === 'dev' ? 72 : 6); index++) {
      const root = add(channel);
      // Keep an actual code example in the latest window; plain fallback text
      // must not be used as the monospace/code-frame fixture.
      if (index === (channel.name === 'dev' ? 71 : 5)) {
        root.body += '\n\n```js\nconst example = 1;\n```';
        root.bodyHtml += '<pre><code>const example = 1;</code></pre>';
      }
      if (index % 12 === 0) { add(channel, root); add(channel, root); root.replyCount = '2'; }
    }
  }
  const last = events.at(-1);
  const project = {id:id(1), checkpoint:{projectId:id(1), seq:last.seq, eventId:last.eventId}};
  const agents = [0, 1, 2].map(index => ({kind:'AGENT',id:id(3 + index),
    handle:['violet-fox', 'blue-otter', 'green-wren'][index], joined:true, host:index === 1 ? 'claude' : 'codex'}));
  const index = messages.map(message => ({id:message.id,channel:message.channel,rootId:message.rootId,seq:Number(message.creationSeq)}));
  return {DevEventBudget:[{id:"budget",events:events.length}], Project:[project], Channel:channels, Agent:agents, Actor:agents.map(agent => ({...agent,chatId:'dev-'+agent.id})), Message:messages, DevMessageIndex:index, Change:events,
    DevReceipt:receiptFixtures(messages,agents,id)};
}

/** Only editable scaffold files are authored; generated ownership records stay intact. */
export async function prepareMock(binary, directory, web, origin, signal) {
  const schema = await readFile(join(web, 'graphql/schema.graphql'), 'utf8');
  const mockSchema = schema + '\nextend type Mutation { devPost(channel: String!, body: String!): ChangeBatch! }\ntype DevRequest { id: ID!, operation: String!, arguments: String!, resultId: ID! }\ntype DevMembership { id: ID!, channel: String! }\ntype DevMessageIndex { id: ID!, channel: String!, rootId: ID, seq: Int! }\ntype DevReceipt { id: ID!, messageId: ID!, sequence: Int!, receipt: RecipientReceipt! }\ntype DevEventBudget { id: ID!, events: Int! }\n';
  await writeFile(join(directory, 'schema.graphql'), mockSchema);
  const project = join(directory, 'project');
  await run(binary, ['mock', 'init', '--schema', join(directory, 'schema.graphql'), '--config-dir', project],
    {signal:AbortSignal.any([signal, AbortSignal.timeout(30_000)])});
  const selector = pubsubSelector(await readFile(join(project, 'generated/types.d.luau'), 'utf8'));
  const config = join(project, 'mock.luau');
  const configuration = await readFile(config, 'utf8');
  if (!configuration.includes('127.0.0.1:4000') || !configuration.includes('allowed_origins = {}')) {
    throw new Error('Unsupported Chigiri Mock server scaffold; expected explicit loopback/origin settings.');
  }
  await writeFile(config, configuration
    .replace('127.0.0.1:4000', '127.0.0.1:0').replace('allowed_origins = {}', `allowed_origins = {${JSON.stringify(origin)}}`));
  for (const [model, rows] of Object.entries(fixtures())) {
    await writeFile(join(project, 'seeds', `${model}.jsonl`), rows.map(row => JSON.stringify(row)).join('\n') + '\n');
  }
  const bodies = {
    'query/project': 'return ctx.models.Project:find({})[1]',
    'query/agents': 'return {items = ctx.models.Agent:find({limit = 50}), nextCursor = ctx.null, scanCursor = head.seq, head = head.seq}',
    'query/channelMembers': `local agents = ctx.models.Actor:find({limit = 50})
      local items: {ChigiriActorRecord} = {}
      for _, agent in agents do
        if agent.kind == "AGENT" and agent.joined and inChannel(args.channel, agent.handle) then
          table.insert(items, agent)
        end
      end
      if ctx.models.DevMembership:find({where = {field = "channel", op = "eq", value = args.channel}})[1] ~= nil then
        local user = ctx.models.Actor:find({where = {field = "id", op = "eq", value = "00000000-0000-0000-0000-000000000002"}})[1]
        if user ~= nil then table.insert(items, user) end
      end
      return {items = items, nextCursor = ctx.null, scanCursor = head.seq, head = head.seq}`,
    'query/channels': `local items: {ChigiriChannelViewRecord} = {}
      local after = if type(args.after) == "string" then tonumber(args.after) or 0 else 0
      for _, channel in ctx.models.Channel:find({limit = 50}) do
        if tonumber(channel.creationSeq) > after and args.state ~= "ARCHIVED" then
          table.insert(items, {id = channel.id, name = channel.name, creationSeq = channel.creationSeq,
            lastConversationAt = "2026-10-24T12:00:00.000Z", archiveAt = "2026-11-23T12:00:00.000Z",
            archived = false, mandatory = channel.name == "dev"})
        end
      end
      table.sort(items, function(a, b) return tonumber(a.creationSeq) < tonumber(b.creationSeq) end)
      return {items = items, nextCursor = ctx.null, scanCursor = head.seq, head = head.seq,
        asOf = "2026-10-24T12:00:00.000Z"}`,
    'query/roots': window.replace('ROOT_CONDITION', '{field = "rootId", op = "isNull", value = true}'),
    'query/replies': window.replace('ROOT_CONDITION', '{field = "rootId", op = "eq", value = args.rootId}'),
    'query/pinnedRoot': `local snapshot = head
      if type(args.snapshot) == "table" then snapshot = checkpoint(args.snapshot.seq) end
      for _, message in ctx.models.Message:find({where = {field = "id", op = "eq", value = args.rootId}, limit = 1}) do
        if message.id == args.rootId and message.channel == args.channel then return {snapshot = snapshot, root = message} end
      end
      return ctx.fail({code = "NOT_FOUND", message = "Development root not found."})`,
    'query/message': messageDetail,
    'query/changes': changes,
    'mutation/createChannel': createChannel.replace('PUBSUB_PUBLISH', `ctx.pubsub.publish[${selector}]`),
    'mutation/postMessage': postMessage.replace('PUBSUB_PUBLISH', `ctx.pubsub.publish[${selector}]`),
    'mutation/replyMessage': replyMessage.replace('PUBSUB_PUBLISH', `ctx.pubsub.publish[${selector}]`),
    'mutation/devPost': post.replace('PUBSUB_PUBLISH', `ctx.pubsub.publish[${selector}]`),
    'subscription/changes': `return ctx.pubsub.subscribe[${selector}]({topic = "viewer-dev"})`,
  };
  for (const [path, body] of Object.entries(bodies)) {
    const file = join(project, 'resolvers', `${path}.luau`), source = await readFile(file, 'utf8');
    const prefix = path.startsWith('subscription/') ? '' : common;
    const marker = source.indexOf('        -- Signature:');
    const end = source.lastIndexOf('    end,');
    if (marker < 0 || end <= marker) throw new Error(`Unsupported Chigiri scaffold: ${path}`);
    await writeFile(file, source.slice(0, marker) + prefix + '\n' + body + '\n' + source.slice(end));
  }
  return project;
}
