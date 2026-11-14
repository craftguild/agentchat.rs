import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import { Link } from 'react-router';
import { Bot, House, MessagesSquare, Plus } from 'lucide-react';
import type { Agent, ChannelView } from '../types.ts';
import { routePath } from '../routes.ts';
import { useSession } from '../state/session.tsx';
import { unreadCounts } from '../state/reading.ts';
import { buildLabel } from '../build-info.ts';
import { ErrorState, IconButton } from './primitives.tsx';

type Section='active'|'archived'|'agents';
type Entry={key:string}&({kind:'heading'|'empty';section:Section}|{kind:'channel';channel:ChannelView}|{kind:'agent';agent:Agent});
export interface SidebarProps {onCreate:()=>void;onNavigate:()=>void;focusSelected:boolean}
/** Both directories retain one list through membership and display-state updates. */
export function Sidebar({onCreate,onNavigate,focusSelected}:SidebarProps){
  const session=useSession(),{directory}=session,scroll=useRef<HTMLDivElement>(null),[version,setVersion]=useState('');
  const heights=useRef(new Map<string,number>()),focused=useRef(false);
  const joined=useMemo(()=>directory.agents.filter(a=>a.joined),[directory.agents]);
  const entries=useMemo(()=>{
    const result:Entry[]=[];
    for(const section of ['active','archived'] as const){
      result.push({key:`heading:${section}`,kind:'heading',section});
      const channels=directory.channels.filter(c=>c.archived===(section==='archived'));
      result.push(...channels.map(channel=>({key:channel.id,kind:'channel' as const,channel})));
      if(!channels.length)result.push({key:`empty:${section}`,kind:'empty',section});
    }
    result.push({key:'heading:agents',kind:'heading',section:'agents'});
    result.push(...joined.map(agent=>({key:agent.id,kind:'agent' as const,agent})));
    if(!joined.length)result.push({key:'empty:agents',kind:'empty',section:'agents'});
    return result;
  },[directory.channels,joined]);
  useEffect(()=>{const keys=new Set(entries.map(entry=>entry.key));for(const key of heights.current.keys())if(!keys.has(key))heights.current.delete(key);},[entries]);
  const estimateSize=(index:number)=>entries[index]!.kind==='heading'?(entries[index] as {section:Section}).section==='active'?64:44:46;
  const virtualizer=useVirtualizer<HTMLDivElement,HTMLDivElement>({count:entries.length,getScrollElement:()=>scroll.current,
    getItemKey:index=>entries[index]!.key,estimateSize,overscan:4,
    measureElement:(element,entry)=>{
      // Hidden rows have zero geometry; retain useful measurements until navigation reopens.
      const height=entry?.borderBoxSize?.[0]?.blockSize??element.offsetHeight,key=element.dataset.key!;
      if(height>0)heights.current.set(key,height);
      return heights.current.get(key)??estimateSize(Number(element.dataset.index));
    }});
  const items=virtualizer.getVirtualItems();
  useLayoutEffect(()=>{
    if(!focusSelected){focused.current=false;return;}
    if(focused.current||!scroll.current?.clientHeight)return;
    const index=entries.findIndex(e=>e.kind==='channel'&&e.channel.name===session.route?.channel);
    if(index>=0)virtualizer.scrollToIndex(index,{align:'auto'});
    // A hidden virtual list can regain its viewport before its selected row is mounted.
    const target=scroll.current.querySelector<HTMLElement>('[aria-current="page"]')??(index<0?scroll.current.querySelector<HTMLElement>('button,a[href]'):null);
    if(target){target.focus({preventScroll:true});focused.current=document.activeElement===target;}
  },[focusSelected,items,entries,session.route?.channel,virtualizer]);
  useEffect(()=>{
    const abort=new AbortController();void fetch('/build-info.txt',{signal:abort.signal,cache:'no-store'}).then(async response=>{
      if(!response.ok)throw new Error('Unavailable');setVersion(buildLabel(await response.json()));
    }).catch(()=>{if(!abort.signal.aborted)setVersion('Version unavailable');});return()=>abort.abort();
  },[]);
  const unread=session.reading?unreadCounts(session.reading):new Map<string,number>();
  const retryAgents=<button className="text-violet-600" onClick={()=>void directory.refreshAgents()}>Retry</button>;
  const render=(entry:Entry)=>{
    if(entry.kind==='heading')return <div role="heading" aria-level={2} data-section={entry.section}
      className={`label ${entry.section} flex w-full min-w-0 items-center justify-between pb-3 pl-6 text-sm leading-5 font-semibold text-slate-400 max-[700px]:pl-4 ${entry.section==='active'?'pt-6 pr-3':'pt-3 pr-6'}`}>
      {entry.section==='active'?'Active':entry.section==='archived'?'Archived':'Agents'}
      {entry.section==='active'&&<IconButton size="sm" aria-label="Create channel" title="Create channel" onClick={onCreate}><Plus size={16} aria-hidden="true"/></IconButton>}
    </div>;
    if(entry.kind==='empty')return <p className="status m-0 px-6 py-3 text-sm leading-5 text-slate-400 max-[700px]:px-4">
      {entry.section==='agents'?(directory.agentsError?<ErrorState compact message="Agents unavailable.">{retryAgents}</ErrorState>:directory.agentsBusy?'Loading agents…':'No participating agents')
        :directory.channelsBusy?'Loading channels…':`No ${entry.section} channels`}
    </p>;
    if(entry.kind==='agent'){
      const muted=directory.members!==null&&!directory.members.ids.has(entry.agent.id),color=muted?'text-slate-300':'text-slate-400';
      return <div className="agent w-full min-w-0 px-3 py-[3px]" data-agent-id={entry.agent.id}>
        <div className={`item flex w-full min-w-0 items-center gap-2.5 rounded-md px-3 py-2.5 text-sm ${muted?'muted text-slate-300':'text-slate-500'}`}
          aria-description={directory.members===null?'Channel membership unknown':`${muted?'Not a member of':'Member of'} #${session.route?.channel}`}>
          <Bot size={16} className={`icon shrink-0 ${color}`} aria-hidden="true"/><span className="min-w-0 truncate">{entry.agent.handle}</span>
          <span className={`separator shrink-0 ${color}`} aria-hidden="true">･</span><span className={`detail min-w-0 max-w-[45%] shrink-0 truncate ${color}`}>{entry.agent.host}</span>
        </div>
      </div>;
    }
    if(entry.kind!=='channel')return null;
    const channel=entry.channel,active=channel.name===session.route?.channel,count=unread.get(channel.id)??0;
    return <div className="channel-row w-full min-w-0 px-3 py-[3px]">
      <Link to={routePath(channel.name)} aria-current={active?'page':undefined} aria-description={channel.mandatory?'Default channel':undefined}
        aria-label={count?`${channel.name} (${count} unread)`:channel.name} onClick={event=>{if(!event.metaKey&&!event.ctrlKey&&!event.shiftKey&&!event.altKey&&event.button===0)onNavigate();}}
        className={`item flex w-full min-w-0 items-center gap-2.5 rounded-md px-3 py-2.5 text-left text-sm no-underline ${active?'active bg-violet-100/70 font-bold text-violet-600':'text-slate-500 hover:bg-violet-50/50'}`}>
        {channel.mandatory?<House size={16} aria-hidden="true" className={`shrink-0 ${active?'text-violet-400':'text-slate-400'}`}/>:<MessagesSquare size={16} aria-hidden="true" className={`shrink-0 ${active?'text-violet-400':'text-slate-400'}`}/>}
        <span className="min-w-0 flex-1 truncate">{channel.name}</span>
        {!!count&&<span className="unread shrink-0 rounded-full bg-violet-100 px-2 py-0.5 text-xs font-semibold text-violet-600 tabular-nums" aria-hidden="true">{new Intl.NumberFormat().format(count)}</span>}
      </Link>
    </div>;
  };
  return <>
    <div ref={scroll} data-testid="navigation-list" className="min-h-0 flex-1 overflow-auto" aria-label="Channels and project agents">
      <div className="relative w-full" style={{height:virtualizer.getTotalSize()}}>{items.map(item=>{
        const entry=entries[item.index]!;return <div key={entry.key} ref={virtualizer.measureElement} data-index={item.index} data-key={entry.key}
          className="absolute top-0 left-0 w-full" style={{transform:`translateY(${item.start}px)`}}>{render(entry)}</div>;
      })}</div>
    </div>
    {directory.agentsError&&!!joined.length&&<div className="px-6 py-3"><ErrorState compact message="Agents unavailable.">{retryAgents}</ErrorState></div>}
    {directory.membersError&&<div className="membership px-6 py-3"><ErrorState compact message="Membership unavailable."><button className="text-violet-600" onClick={directory.refreshMembers}>Retry</button></ErrorState></div>}
    <span role="status" className="sr-only">{directory.membersBusy?'Updating channel membership…':''}</span>
    <footer aria-label="Build version" className="shrink-0 truncate border-t border-slate-200 px-6 py-3 text-sm text-slate-400 max-[700px]:px-4">{version||'Loading version…'}</footer>
  </>;
}
