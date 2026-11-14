/** Publish one bounded message to the isolated development Mock, never AgentChat. */
import { parseArgs } from 'node:util';

try {
  const {values, positionals} = parseArgs({allowPositionals:true, options:{channel:{type:'string', default:'dev'}}});
  const body = positionals.join(' ') || 'Hello from Chigiri Mock — ライブ更新';
  const port = Number(process.env.AGENTCHAT_DEV_PORT ?? 5173);
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('AGENTCHAT_DEV_PORT must be 1..65535.');
  const response = await fetch(`http://127.0.0.1:${port}/graphql`, {method:'POST',
    headers:{'Content-Type':'application/json', Accept:'application/graphql-response+json'},
    body:JSON.stringify({query:'mutation($channel:String!,$body:String!){ devPost(channel:$channel,body:$body){ scan { seq } } }',
      variables:{channel:values.channel, body}}), signal:AbortSignal.timeout(10_000)});
  const result = await response.json();
  if (!response.ok || result.errors) throw new Error(JSON.stringify(result.errors ?? result));
  console.log(`Published to Mock #${values.channel}, seq=${result.data.devPost.scan.seq}.`);
} catch (error) { console.error(error.message); process.exitCode = 1; }
