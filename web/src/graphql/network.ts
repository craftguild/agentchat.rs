/** Finite JSON and SSE are decoded before Apollo can publish or cache data. */
import { ApolloLink } from '@apollo/client';
import { Observable } from 'rxjs';
import { print } from 'graphql';
import type { FormattedExecutionResult } from 'graphql';
import { events, MAX_FRAME_BYTES } from '../sse.ts';
import { same, WireError } from '../types.ts';
import type { Checkpoint } from '../types.ts';
import * as decode from '../validate.ts';

const retryCodes = new Set(['busy','runtime_unavailable','io_error','storage_error',
  'MOCK_RESOURCE_LIMIT','MOCK_ENVIRONMENT','MOCK_EXECUTION_LIMIT_EXCEEDED','MOCK_EXECUTION_TIMEOUT','MOCK_EXECUTION_FAILED']);
const actionErrors:Record<string,string> = {
  invalid_name:'Use a valid channel name.',invalid_argument:'Check the channel and message text.',
  invalid_thread:'This thread is unavailable.',not_found:'The channel or thread no longer exists.',
  not_joined:'Could not join this channel. Retry your action.',
  idempotency_conflict:'This request identifies a different action. Edit the text before sending again.',
  DEV_LIMIT:'The development fixture is full or the message exceeds its 768-byte limit. Restart dev to reset it.',
};

/** Envelopes with errors or any reported loss never yield partial data. */
export function envelope(value:unknown, writing=false):FormattedExecutionResult {
  const result=decode.record(value);
  if (Array.isArray(result.errors) && result.errors.length) {
    const error=decode.record(result.errors[0]), ext=error.extensions ? decode.record(error.extensions) : {};
    const code=typeof ext.code==='string' ? ext.code : 'request_failed', reset=ext.resetRequired===true;
    throw new WireError(writing ? actionErrors[code] ?? 'Could not complete the action. Retry to check its original result.'
      : reset ? 'History changed. Reload the snapshot.' : code==='project_binding_mismatch'
      ? 'The project changed. Restart the viewer.' : 'Viewer data is temporarily unavailable.',code,reset,retryCodes.has(code));
  }
  const ext=result.extensions == null ? {} : decode.record(result.extensions);
  const chigiri=ext.chigiri == null ? {} : decode.record(ext.chigiri);
  if (Object.hasOwn(chigiri,'delivery') && decode.record(chigiri.delivery).droppedTotal!==null) {
    throw new WireError('Live delivery was interrupted. Replaying…','delivery_loss',false,true);
  }
  decode.record(result.data);
  return result as FormattedExecutionResult;
}
async function json(response:Response,signal:AbortSignal):Promise<unknown> {
  const reader=response.body!.getReader(), decoder=new TextDecoder('utf-8',{fatal:true});
  const abort=()=>{void reader.cancel().catch(()=>{});};
  signal.addEventListener('abort',abort,{once:true});
  let text='', bytes=0;
  try {
    for (;;) {
      signal.throwIfAborted();const next=await reader.read();signal.throwIfAborted();bytes+=next.value?.byteLength ?? 0;
      if (bytes>MAX_FRAME_BYTES) throw new WireError('Viewer response exceeded its budget.','response_overflow',true);
      text+=decoder.decode(next.value,{stream:!next.done});
      if (next.done) return JSON.parse(text);
    }
  } finally { signal.removeEventListener('abort',abort);await reader.cancel().catch(()=>{}); reader.releaseLock(); }
}
async function pause(ms:number,signal:AbortSignal):Promise<void> {
  await new Promise<void>((resolve,reject)=>{
    signal.throwIfAborted();
    const timer=setTimeout(()=>{signal.removeEventListener('abort',abort);resolve();},ms);
    const abort=()=>{clearTimeout(timer);reject(signal.reason);};
    signal.addEventListener('abort',abort,{once:true});
  });
}

/** Each subscription has one fetch; its session owns reconnect and checkpointing. */
export function viewerLink():ApolloLink {
  return new ApolloLink(operation=>new Observable(observer=>{
    const controller=new AbortController(), signal=controller.signal;
    const context=operation.getContext() as {signal?:AbortSignal;connected?:()=>void;closed?:()=>void;check?:(data:unknown)=>void};
    const abort=()=>controller.abort(context.signal?.reason);
    context.signal?.addEventListener('abort',abort,{once:true});
    if (context.signal?.aborted) abort();
    const definition=operation.query.definitions.find(d=>d.kind==='OperationDefinition');
    const kind=definition?.kind==='OperationDefinition' ? definition.operation : 'query';
    void (async()=>{
      for (let attempt=0;;attempt++) {
        try {
          const response=await fetch('/graphql',{method:'POST',signal,credentials:'same-origin',cache:'no-store',
            headers:{'Content-Type':'application/json',Accept:kind==='subscription' ? 'text/event-stream' : 'application/graphql-response+json'},
            body:JSON.stringify({query:print(operation.query),variables:operation.variables,operationName:operation.operationName})});
          if (!response.ok) {
            await response.body?.cancel();
            throw new WireError('Connection interrupted. Reconnecting…',`http_${response.status}`,false,response.status>=500 || response.status===429);
          }
          if (!response.body) throw new WireError('Missing viewer response.','empty_response',false,true);
          if (kind!=='subscription') {
            const result=envelope(await json(response,signal),kind==='mutation'); context.check?.(result.data);
            observer.next(result); observer.complete(); return;
          }
          if (!response.headers.get('content-type')?.startsWith('text/event-stream')) {
            envelope(await json(response,signal)); throw new WireError('Expected a live stream.','invalid_stream',true);
          }
          context.connected?.();
          let through:Checkpoint|null=null;
          for await (const event of events(response.body,signal)) {
            if (event.event==='complete') throw new WireError('Live connection completed. Reconnecting…','source_complete',false,true);
            const result=envelope(event.data),batch=decode.batch(decode.record(result.data).changes);
            // A paged replay has one immutable head, even when each individual
            // frame is otherwise valid. Reject a changed range before publication.
            if(through&&!same(through,batch.head))throw new WireError('Live history changed during replay.','changed_head',true);
            through=batch.hasMore?batch.head:null;
            context.check?.(result.data);
            observer.next(result);
          }
          throw new WireError('Live connection ended. Reconnecting…','connection_ended',false,true);
        } catch (error) {
          if (signal.aborted) throw signal.reason;
          if (kind!=='query' || attempt>=2 || !(error instanceof WireError) || !error.retry || error.reset) throw error;
          await pause([100,300][attempt]!,signal);
        }
      }
    })().catch(error=>observer.error(error)).finally(()=>context.closed?.());
    return ()=>{context.signal?.removeEventListener('abort',abort);controller.abort();};
  }));
}
