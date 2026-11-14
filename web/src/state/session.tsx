import { createContext, useCallback, useContext, useEffect, useMemo, useReducer, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { useLocation, useNavigate } from 'react-router';
import { useSdk } from '../graphql/provider.tsx';
import { parseRoute, routePath } from '../routes.ts';
import type { ViewRoute } from '../routes.ts';
import { WireError } from '../types.ts';
import type { ChangeBatch, Checkpoint, Project } from '../types.ts';
import { useDirectory } from './directory.ts';
import { useLive } from './use-live.ts';
import type { ConnectionState } from './use-live.ts';
import { startReading, receiveReading, markReading, expireReading, nextExpiry } from './reading.ts';
import type { Post, Reading } from './reading.ts';
import { emptyTimeline, ingestTimeline, prependTimeline, targetTimeline, viewKey } from './timeline.ts';
import type { Anchor, Timeline } from './timeline.ts';
import { loadHistory } from './history.ts';

interface SessionState {
  project:Project|null;checkpoint:Checkpoint|null;reading:Reading|null;views:ReadonlyMap<string,Timeline>;
  route:ViewRoute|null;routeBusy:boolean;routeError:string;connection:ConnectionState;notice:string;version:number;
}
export interface Position {anchor:Anchor|null;heights:Map<string,number>}
type Loading={controller:AbortController;pending:ChangeBatch[];bytes:number;promise:Promise<void>};
const initial=():SessionState=>({project:null,checkpoint:null,reading:null,views:new Map(),route:null,routeBusy:false,routeError:'',connection:'connecting',notice:'',version:0});

function useSessionValue(){
  const resources=useSdk(),{sdk,contents,client}=resources;
  const [state,dispatch]=useReducer((_old:SessionState,next:SessionState)=>next,undefined,initial);
  const current=useRef(state),loads=useRef(new Map<string,Loading>()),positions=useRef(new Map<string,Position>());
  const reveals=useRef(new Map<string,string>()),[epoch,setEpoch]=useState(0),selection=useRef(0);
  const {pathname}=useLocation(),navigate=useNavigate();
  const publish=useCallback((next:SessionState)=>{current.current=next;dispatch(next);},[]);
  const patch=useCallback((value:Partial<SessionState>)=>publish({...current.current,...value}),[publish]);
  const setView=useCallback((key:string,view:Timeline)=>{
    const views=new Map(current.current.views);views.delete(key);views.set(key,view);
    if(views.size>8){
      const active=current.current.route;
      const protectedKeys=new Set(active ? [viewKey(active.channel,null),viewKey(active.channel,active.root)] : []);
      for(const old of views.keys())if(views.size>8 && old!==key && !protectedKeys.has(old)){
        views.delete(old);positions.current.delete(old);loads.current.get(old)?.controller.abort();loads.current.delete(old);
      }
    }
    patch({views});
  },[patch]);
  const progress=useCallback(()=>{const at=current.current.checkpoint;if(!at)throw new Error('Missing project snapshot');return at;},[]);
  const fail=useCallback((error:unknown)=>{
    if(error instanceof WireError && error.reset)patch({connection:'reset',notice:error.message});
  },[patch]);
  const load=useCallback(async(channel:string,root:string|null,older=false)=>{
    const key=viewKey(channel,root),existing=current.current.views.get(key);
    const prior=loads.current.get(key);if(prior)return prior.promise;
    if(older && (!existing?.before || existing.busy))return;
    const view=existing ?? emptyTimeline(channel,root,progress());
    setView(key,{...view,busy:true,error:'',lastLoad:older?'older':'newest'});
    const owner:Loading={controller:new AbortController(),pending:[],bytes:0,promise:Promise.resolve()};
    loads.current.set(key,owner);
    owner.promise=(async()=>{
      try {
        let result=await loadHistory(sdk,contents,channel,root,older ? view.before : null,older ? view.snapshot : null,progress,owner.pending,owner.controller.signal);
        if(owner.controller.signal.aborted || loads.current.get(key)!==owner)return;
        if(older)result=prependTimeline(current.current.views.get(key) ?? view,result);
        const reveal=reveals.current.get(key);
        if(reveal && result.rows.some(row=>row.message.id===reveal)){result=targetTimeline(result,reveal);reveals.current.delete(key);}
        if(!older)positions.current.set(key,{anchor:null,heights:positions.current.get(key)?.heights ?? new Map()});
        setView(key,{...result,busy:false,error:''});
      }catch(error){
        if(!owner.controller.signal.aborted && loads.current.get(key)===owner){
          fail(error);setView(key,{...(current.current.views.get(key) ?? view),busy:false,
            error:error instanceof WireError && error.code==='not_found' ? 'Thread not found.' : (error as Error).message});
        }
      }finally{if(loads.current.get(key)===owner)loads.current.delete(key);}
    })();
    return owner.promise;
  },[sdk,contents,progress,setView,fail]);
  useEffect(()=>{
    const controller=new AbortController();selection.current++;
    for(const owner of loads.current.values())owner.controller.abort();loads.current.clear();
    positions.current.clear();reveals.current.clear();contents.clear();publish(initial());
    void client.clearStore().then(()=>sdk.Project(undefined,{signal:controller.signal})).then(({project})=>{
      if(!controller.signal.aborted)patch({project,checkpoint:project.checkpoint,reading:startReading(project.checkpoint)});
    }).catch(error=>{if(!controller.signal.aborted)patch({connection:'error',notice:(error as Error).message,routeError:(error as Error).message});});
    return()=>{controller.abort();for(const owner of loads.current.values())owner.controller.abort();loads.current.clear();};
  },[sdk,client,contents,epoch,publish,patch]);
  const apply=useCallback((batch:ChangeBatch)=>{
    const old=current.current;if(!old.reading)return;
    const reading=receiveReading(old.reading,batch),views=new Map<string,Timeline>();
    for(const [key,value] of old.views){
      let view=ingestTimeline({...value,anchor:positions.current.get(key)?.anchor ?? value.anchor},batch);
      const reveal=reveals.current.get(key);
      if(reveal && view.rows.some(row=>row.message.id===reveal)){view=targetTimeline(view,reveal);reveals.current.delete(key);}
      views.set(key,view);
    }
    const bytes=batch.items.reduce((n,item)=>n+(item.kind==='MESSAGE_CREATED' ? 2*(item.message.body.length+item.message.bodyHtml.length)+1024 : 512),1024);
    for(const owner of loads.current.values())if(owner.bytes+bytes>2*1024*1024 || owner.pending.length>=256)throw new WireError('History setup exceeded its buffer.','view_overflow',true);
    // No progress is published until every validation and bounded-state transition succeeds.
    client.cache.batch({update:()=>{for(const item of batch.items)if(item.kind==='MESSAGE_CREATED')contents.put(item.message);}});
    for(const owner of loads.current.values()){owner.pending.push(batch);owner.bytes+=bytes;}
    publish({...old,reading,views,checkpoint:batch.scan,version:old.version+1});
  },[client,contents,publish]);
  const status=useCallback((connection:ConnectionState,notice='')=>patch({connection,notice}),[patch]);
  useLive({enabled:!!state.project,sdk,progress,apply,status});
  const directory=useDirectory(!!state.project,state.route?.channel ?? '',state.version+epoch);
  const parsed=parseRoute(pathname),chosen=directory.channels.find(c=>c.name===parsed?.channel)
    ?? (!parsed?.channel ? directory.channels.find(c=>c.mandatory) ?? directory.channels[0] : undefined);
  const listingReady=!directory.channelsBusy || !!directory.channels.length;
  useEffect(()=>{
    if(!state.project || !listingReady)return;
    const generation=++selection.current;
    if(!parsed){patch({routeError:'Invalid conversation location.',routeBusy:false});return;}
    if(!chosen){patch({routeError:directory.channelsError || 'Channel not found.',routeBusy:false});return;}
    if(!parsed.channel){navigate(routePath(chosen.name),{replace:true});return;}
    patch({routeBusy:true,routeError:''});
    void (async()=>{
      const mainKey=viewKey(chosen.name,null);
      if(!current.current.views.has(mainKey))await load(chosen.name,null);
      if(parsed.root && !current.current.views.has(viewKey(chosen.name,parsed.root)))await load(chosen.name,parsed.root);
      if(generation===selection.current)patch({route:{channel:chosen.name,root:parsed.root},routeBusy:false});
    })();
    return()=>{selection.current++;};
  // Listing refreshes cannot restart an in-flight route for the same channel ID.
  },[pathname,state.project?.id,listingReady,chosen?.id,load,navigate,patch]);
  const expiry=state.reading ? nextExpiry(state.reading) : null;
  useEffect(()=>{
    if(expiry===null)return;
    const timer=setTimeout(()=>{const reading=current.current.reading;if(reading)patch({reading:expireReading(reading,Date.now())});},Math.max(1,expiry-Date.now()));
    return()=>clearTimeout(timer);
  },[expiry,patch]);
  const actions=useMemo(()=>({
    reset:()=>setEpoch(n=>n+1),load,
    go:(channel:string,root:string|null=null)=>navigate(routePath(channel,root)),
    position:(key:string):Position=>{let value=positions.current.get(key);if(!value){value={anchor:null,heights:new Map()};positions.current.set(key,value);}return value;},
    completeTarget:(key:string,version:number)=>{const view=current.current.views.get(key);if(view?.target?.version===version)setView(key,{...view,target:null});},
    cancelTarget:(key:string)=>{reveals.current.delete(key);const view=current.current.views.get(key);if(view?.target)setView(key,{...view,target:null});},
    mark:(posts:Post[])=>{const reading=current.current.reading;if(reading){try{patch({reading:markReading(reading,posts,Date.now())});}catch(error){fail(error);}}},
    reveal:(channel:string,root:string|null,id:string)=>{
      const key=viewKey(channel,root),view=current.current.views.get(key);
      if(view?.rows.some(row=>row.message.id===id))setView(key,targetTimeline(view,id));else reveals.current.set(key,id);
    },
    fail,
  }),[load,navigate,patch,setView,fail]);
  return {...state,directory,actions,epoch};
}
export type Session=ReturnType<typeof useSessionValue>;
const Context=createContext<Session|null>(null);
/** React state owns view projections and interaction; Apollo owns server content. */
export function SessionProvider({children}:{children:ReactNode}) {
  const value=useSessionValue();return <Context.Provider value={value}>{children}</Context.Provider>;
}
export function useSession():Session {const value=useContext(Context);if(!value)throw new Error('Missing SessionProvider');return value;}
