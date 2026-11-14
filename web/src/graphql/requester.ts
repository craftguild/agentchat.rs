import type { ApolloClient, OperationVariables } from '@apollo/client';
import { getOperationAST } from 'graphql';
import type { DocumentNode } from 'graphql';
import { Observable } from 'rxjs';
import type { Requester } from './catalog.generated.ts';
import { checkResult } from './validation.ts';
import { WireError } from '../types.ts';

/** Cancellation belongs to the caller; live teardown can be awaited separately. */
export interface RequestContext {
  signal?:AbortSignal;
  connected?:()=>void;
  closed?:()=>void;
}

/** Bind every generated operation to the same bounded Apollo link and cache policy. */
export function createApolloRequester(client:ApolloClient):Requester<RequestContext> {
  return <R,V>(document:DocumentNode,variables?:V,options:RequestContext={}):Promise<R>|Observable<R>=>{
    const operation=getOperationAST(document);
    if(!operation)throw new WireError('Missing GraphQL operation.','invalid_operation',true);
    const values=(variables??{}) as OperationVariables;
    const context={...options,check:(data:unknown)=>checkResult(operation,values,data)};
    if(operation.operation==='subscription')return new Observable<R>(subscriber=>{
      const source=client.subscribe({query:document,variables:values,fetchPolicy:'no-cache',context}).subscribe({
        next:result=>{
          // Apollo 4 can emit an error result followed by completion. Convert it
          // back to the Observable error channel without changing its policy.
          if(result.error){subscriber.error(result.error);return;}
          subscriber.next(result.data as R);
        },error:error=>subscriber.error(error),complete:()=>subscriber.complete(),
      });
      return()=>source.unsubscribe();
    });
    return (async()=>{
      if(operation.operation==='mutation'){
        const result=await client.mutate({mutation:document,variables:values,context});
        if(result.error)throw result.error;
        return result.data as R;
      }
      // Cursor windows must not accumulate in a second cache. Immutable content
      // has its own byte-bounded normalized cache, managed by createContentCache.
      const result=await client.query({query:document,variables:values,fetchPolicy:'no-cache',context});
      options.signal?.throwIfAborted();if(result.error)throw result.error;
      return result.data as R;
    })();
  };
}
