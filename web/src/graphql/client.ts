import { ApolloClient, InMemoryCache } from '@apollo/client';
import { RemoveTypenameFromVariablesLink } from '@apollo/client/link/remove-typename';
import { viewerLink } from './network.ts';
import { CachedBodyFragmentDoc } from './catalog.generated.ts';
import type { CachedBodyFragment } from './catalog.generated.ts';
import type { Message } from '../types.ts';

export const BODY_BYTES=64*1024*1024;
export type Body=Pick<Message,'body'|'bodyHtml'|'renderFallback'>;

/** One fixed-project client. Snapshot projections never normalize by message ID. */
export function createViewerClient():ApolloClient {
  return new ApolloClient({link:new RemoveTypenameFromVariablesLink().concat(viewerLink()),queryDeduplication:false,
    // The byte LRU is the sole content-retention policy. Result memoization
    // could otherwise retain evicted fragment strings outside that budget.
    cache:new InMemoryCache({resultCaching:false,typePolicies:{
      Message:{keyFields:false},MessageWindow:{keyFields:false},PinnedRoot:{keyFields:false},
      ChangeBatch:{keyFields:false},Change:{keyFields:false},Checkpoint:{keyFields:false},
      ViewerBody:{keyFields:['id']},
    }}),
  });
}

/** Only LRU identity/size bookkeeping lives outside Apollo; content lives in its cache. */
export function createContentCache(client:ApolloClient,limit=BODY_BYTES) {
  const normalized=client.cache;
  if(!(normalized instanceof InMemoryCache))throw new Error('Viewer content requires InMemoryCache.');
  const sizes=new Map<string,number>(); let bytes=0;
  const key=(id:string)=>client.cache.identify({__typename:'ViewerBody',id})!;
  const touch=(id:string)=>{const size=sizes.get(id);if(size!==undefined){sizes.delete(id);sizes.set(id,size);}};
  return {
    get bytes(){return bytes;},
    get(id:string):Body | undefined {
      const body=client.cache.readFragment<CachedBodyFragment>({id:key(id),fragment:CachedBodyFragmentDoc});
      if (body) {touch(id);return body;} return undefined;
    },
    has(id:string):boolean {return !!this.get(id);},
    put(message:Message):void {
      const size=2*(message.body.length+message.bodyHtml.length+message.id.length)+256;
      if(size>limit) throw new Error('A complete body exceeds the browser cache budget.');
      const retained=sizes.has(message.id);
      bytes-=sizes.get(message.id) ?? 0;sizes.delete(message.id);
      while(sizes.size && bytes+size>limit){const [id,n]=sizes.entries().next().value!;sizes.delete(id);bytes-=n;client.cache.evict({id:key(id)});normalized.release(key(id));}
      client.cache.writeFragment<CachedBodyFragment & {__typename:'ViewerBody'}>({id:key(message.id),fragment:CachedBodyFragmentDoc,
        data:{__typename:'ViewerBody',id:message.id,body:message.body,bodyHtml:message.bodyHtml,renderFallback:message.renderFallback}});
      // Keep exactly one retained root per LRU entry across replay writes.
      if(retained)normalized.release(key(message.id));
      sizes.set(message.id,size);bytes+=size;
    },
    clear():void {for(const id of sizes.keys()){client.cache.evict({id:key(id)});normalized.release(key(id));}sizes.clear();bytes=0;client.cache.gc();},
  };
}
export type ContentCache=ReturnType<typeof createContentCache>;
