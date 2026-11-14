import { createContext, useContext, useEffect, useLayoutEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { createPortal } from 'react-dom';
import type { Agent } from '../types.ts';
import { Identity } from './identity.tsx';

type Profile={anchor:HTMLElement;agent:Agent};
const Context=createContext<{open:(value:Profile)=>void;close:()=>void;later:()=>void;cancel:()=>void}|null>(null);
/** A single profile overlay serves every virtualized message. */
export function MentionProvider({children}:{children:ReactNode}){
  const [profile,setProfile]=useState<Profile|null>(null),box=useRef<HTMLDivElement>(null),timer=useRef<ReturnType<typeof setTimeout>|null>(null);
  const cancel=()=>{if(timer.current!==null)clearTimeout(timer.current);timer.current=null;};
  const close=()=>{cancel();setProfile(null);};
  useEffect(()=>{
    if(!profile)return;
    profile.anchor.setAttribute('aria-describedby','mention-profile');
    const key=(event:globalThis.KeyboardEvent)=>{if(event.key==='Escape'){event.preventDefault();close();}};
    const changed=new MutationObserver(()=>{if(!profile.anchor.isConnected)close();});changed.observe(document.body,{subtree:true,childList:true});
    document.addEventListener('keydown',key);document.addEventListener('scroll',close,true);window.addEventListener('resize',close);
    return()=>{profile.anchor.removeAttribute('aria-describedby');changed.disconnect();document.removeEventListener('keydown',key);document.removeEventListener('scroll',close,true);window.removeEventListener('resize',close);cancel();};
  },[profile]);
  useLayoutEffect(()=>{
    if(!profile||!box.current)return;
    const anchor=profile.anchor.getBoundingClientRect(),size=box.current.getBoundingClientRect();
    box.current.style.left=`${Math.max(8,Math.min(anchor.left,innerWidth-size.width-8))}px`;
    box.current.style.top=`${Math.max(8,anchor.bottom+8+size.height<=innerHeight-8?anchor.bottom+8:anchor.top-size.height-8)}px`;
  },[profile]);
  return <Context.Provider value={{open:value=>{cancel();setProfile(old=>old?.anchor===value.anchor?old:value);},close,cancel,later:()=>{cancel();timer.current=setTimeout(close,120);}}}>
    {children}{profile && createPortal(<div id="mention-profile" role="tooltip" ref={box} onPointerEnter={cancel} onPointerLeave={close}
      className="profile fixed z-50 max-w-[calc(100vw-16px)] rounded-lg border border-slate-200 bg-white p-3 shadow-lg"><Identity author={profile.agent} host={profile.agent.host}/></div>,document.body)}
  </Context.Provider>;
}
/** Only server-owned mention spans resolve through the retained agent directory. */
export function MessageBody({html,agents}:{html:string;agents:readonly Agent[]}){
  const ref=useRef<HTMLDivElement>(null),profiles=useContext(Context);
  useEffect(()=>{
    for(const span of ref.current?.querySelectorAll<HTMLElement>('.mention[data-mention]') ?? []){
      if(agents.some(a=>a.handle===span.dataset.mention)){span.tabIndex=0;}else span.removeAttribute('tabindex');
    }
  },[html,agents]);
  const open=(target:EventTarget|null)=>{
    const anchor=target instanceof Element?target.closest<HTMLElement>('.mention[data-mention]'):null;
    const agent=agents.find(a=>a.handle===anchor?.dataset.mention);
    if(anchor&&agent&&ref.current?.contains(anchor))profiles?.open({anchor,agent});
  };
  return <div ref={ref} className="message-content min-w-0 text-sm leading-7 wrap-anywhere [hyphens:auto] [word-break:normal]" lang="en"
    onPointerOver={event=>open(event.target)} onPointerOut={()=>profiles?.later()}
    onFocus={event=>open(event.target)} onBlur={()=>profiles?.close()}
    onClick={event=>{const anchor=event.target instanceof Element?event.target.closest('a'):null;if(anchor&&/^(https?:|mailto:)/.test(anchor.href)){anchor.target='_blank';anchor.rel='noopener noreferrer nofollow';}}}
    dangerouslySetInnerHTML={{__html:html}}/>;
}
