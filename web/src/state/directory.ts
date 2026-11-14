import { useCallback, useEffect, useRef, useState } from 'react';
import { useSdk } from '../graphql/provider.tsx';
import { retainedAgents, retainedActors } from '../agents.ts';
import { allChannels } from '../channels.ts';
import * as decode from '../validate.ts';
import { classifyChannel } from '../archive.ts';
import type { Agent, ChannelView } from '../types.ts';

interface Members {ids:ReadonlySet<string>;count:number}
interface Directory {
  channels:ChannelView[];agents:Agent[];members:Members|null;
  channelsBusy:boolean;agentsBusy:boolean;membersBusy:boolean;
  channelsError:string;agentsError:string;membersError:string;
}
const requestState=()=>({channels:null as Promise<void>|null,agents:null as Promise<void>|null,channelsAgain:false,agentsAgain:false});
const initial:Directory={channels:[],agents:[],members:null,channelsBusy:true,agentsBusy:true,membersBusy:false,channelsError:'',agentsError:'',membersError:''};

/** Coalesced finite metadata reads; one live progress notification triggers refresh. */
export function useDirectory(enabled:boolean,channel:string,version:number) {
  const {sdk}=useSdk(),[data,setData]=useState(initial);
  const membersCache=useRef(new Map<string,Members>());
  const clock=useRef({utc:0,monotonic:0});
  const requests=useRef(requestState());
  const controller=useRef(new AbortController());
  const now=useCallback(()=>clock.current.utc+Math.max(0,performance.now()-clock.current.monotonic),[]);
  useEffect(()=>{
    const own=new AbortController();controller.current=own;
    requests.current=requestState();
    if(!enabled){membersCache.current.clear();clock.current={utc:0,monotonic:0};setData(initial);}
    return()=>own.abort();
  },[enabled,sdk]);
  const refreshChannels=useCallback(()=>{
    const signal=controller.current.signal,r=requests.current;
    if(r.channels){r.channelsAgain=true;return r.channels;}
    setData(d=>({...d,channelsBusy:true}));
    r.channels=(async()=>{
      try{do{
        r.channelsAgain=false;
        try{
          const result=await allChannels((after,signal)=>sdk.Channels({after},{signal}).then(data=>data.channels),signal);if(signal.aborted)return;
          clock.current={utc:Date.parse(result.asOf),monotonic:performance.now()};
          setData(d=>({...d,channels:result.items,channelsError:''}));
        }catch(e){if(!signal.aborted)setData(d=>({...d,channelsError:(e as Error).message}));}
      }while(r.channelsAgain&&!signal.aborted);}
      finally{r.channels=null;if(!signal.aborted)setData(d=>({...d,channelsBusy:false}));}
    })();return r.channels;
  },[sdk]);
  const refreshAgents=useCallback(()=>{
    const signal=controller.current.signal,r=requests.current;
    if(r.agents){r.agentsAgain=true;return r.agents;}
    setData(d=>({...d,agentsBusy:true}));
    r.agents=(async()=>{
      try{do{
        r.agentsAgain=false;
        try{const agents=await retainedAgents((after,signal)=>sdk.Agents({after},{signal}).then(data=>decode.agents(data.agents)),signal);if(!signal.aborted)setData(d=>({...d,agents,agentsError:''}));}
        catch(e){if(!signal.aborted)setData(d=>({...d,agentsError:(e as Error).message}));}
      }while(r.agentsAgain&&!signal.aborted);}
      finally{r.agents=null;if(!signal.aborted)setData(d=>({...d,agentsBusy:false}));}
    })();return r.agents;
  },[sdk]);
  useEffect(()=>{if(enabled){void refreshChannels();void refreshAgents();}},[enabled,version,refreshChannels,refreshAgents]);
  const [memberRetry,setMemberRetry]=useState(0);
  useEffect(()=>{
    const abort=new AbortController();
    if(!enabled || !channel){setData(d=>({...d,members:null,membersBusy:false,membersError:''}));return;}
    const cached=membersCache.current.get(channel) ?? null;
    setData(d=>({...d,members:cached,membersBusy:true,membersError:''}));
    void retainedActors((after,signal)=>sdk.Members({channel,after},{signal}).then(data=>data.channelMembers),abort.signal).then(actors=>{
      if(abort.signal.aborted)return;
      const joined=actors.filter(a=>a.joined),members={ids:new Set(joined.map(a=>a.id)),count:joined.filter(a=>a.kind==='AGENT').length};
      membersCache.current.delete(channel);membersCache.current.set(channel,members);
      while(membersCache.current.size>8)membersCache.current.delete(membersCache.current.keys().next().value!);
      setData(d=>({...d,members,membersBusy:false,membersError:''}));
    }).catch(e=>{if(!abort.signal.aborted){membersCache.current.delete(channel);setData(d=>({...d,members:null,membersBusy:false,membersError:(e as Error).message}));}});
    return()=>abort.abort();
  },[enabled,channel,version,memberRetry,sdk]);
  useEffect(()=>{
    if(!enabled || !clock.current.utc)return;
    const deadlines=data.channels.map(c=>Date.parse(c.archiveAt)).filter(t=>t>now());
    const update=()=>setData(d=>({...d,channels:d.channels.map(c=>classifyChannel(c,now()))}));
    const timer=deadlines.length ? setTimeout(update,Math.max(1,Math.min(2_147_483_647,Math.ceil(Math.min(...deadlines)-now())))) : null;
    document.addEventListener('visibilitychange',update);
    return()=>{if(timer!==null)clearTimeout(timer);document.removeEventListener('visibilitychange',update);};
  },[enabled,data.channels,now]);
  return {...data,refreshChannels,refreshAgents,refreshMembers:()=>setMemberRetry(n=>n+1)};
}
