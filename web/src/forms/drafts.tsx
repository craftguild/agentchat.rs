import { createContext, useContext, useEffect, useMemo, useReducer, useRef } from 'react';
import type { ReactNode } from 'react';
import { useSdk } from '../graphql/provider.tsx';
import { useSession } from '../state/session.tsx';
import { viewKey } from '../state/timeline.ts';

export interface Draft {text:string;pending:boolean;error:string;warning:string;attempt:{id:string;body:string}|null}
const empty=():Draft=>({text:'',pending:false,error:'',warning:'',attempt:null});
type Action={key:string;update:Partial<Draft>};
/** Unsent and in-flight drafts cannot be evicted when changing destinations. */
export function draftReducer(state:ReadonlyMap<string,Draft>,action:Action):ReadonlyMap<string,Draft>{
  const next=new Map(state);
  if(!next.has(action.key)){
    for(const [key,value] of next)if(next.size>=32 && !value.pending && !value.text)next.delete(key);
    if(next.size>=32)throw new Error('Too many open drafts. Send or clear an existing draft first.');
  }
  next.set(action.key,{...(next.get(action.key)??empty()),...action.update});return next;
}
function useDraftState(){
  const {sdk}=useSdk(),session=useSession();
  const [drafts,dispatch]=useReducer((_old:ReadonlyMap<string,Draft>,next:ReadonlyMap<string,Draft>)=>next,new Map());
  const current=useRef(drafts),controller=useRef(new AbortController());
  useEffect(()=>{controller.current=new AbortController();return()=>controller.current.abort();},[]);
  const change=(key:string,update:Partial<Draft>)=>{const next=draftReducer(current.current,{key,update});current.current=next;dispatch(next);};
  const actions=useMemo(()=>({
    edit:(key:string,text:string)=>change(key,{text,error:'',warning:''}),
    send:async(channel:string,root:string|null,body:string):Promise<boolean>=>{
      const key=viewKey(channel,root),draft=current.current.get(key)??empty();if(draft.pending)return false;
      const attempt=draft.attempt?.body===body ? draft.attempt : {id:crypto.randomUUID(),body};
      change(key,{text:body,pending:true,error:'',warning:'',attempt});
      try {
        const options={signal:controller.current.signal};
        const {result:posted}=root?await sdk.Reply({requestId:attempt.id,rootId:root,body},options):await sdk.Post({requestId:attempt.id,channel,body},options);
        change(key,{text:'',attempt:null,pending:false,warning:posted.warnings.map(w=>`@${w.handle}: ${w.code.replaceAll('_',' ')}`).join('; ')});
        session.actions.reveal(channel,root,posted.id);return true;
      }catch(error){change(key,{pending:false,error:(error as Error).message});return false;}
    },
  }),[sdk,session.actions]);
  return {drafts,...actions};
}
type DraftState=ReturnType<typeof useDraftState>;
const Context=createContext<DraftState|null>(null);
export function DraftProvider({children}:{children:ReactNode}){const value=useDraftState();return <Context.Provider value={value}>{children}</Context.Provider>;}
export function useDraft(channel:string,root:string|null){
  const value=useContext(Context);if(!value)throw new Error('Missing DraftProvider');
  const key=viewKey(channel,root);return {draft:value.drafts.get(key)??empty(),edit:(text:string)=>value.edit(key,text),send:(body:string)=>value.send(channel,root,body)};
}
