import { useEffect, useRef, useState } from 'react';
import { Folder, Menu } from 'lucide-react';
import { useSession } from './state/session.tsx';
import { viewerDirectory } from './viewer-context.ts';
import { Sidebar } from './ui/sidebar.tsx';
import { Panel } from './ui/panel.tsx';
import { ErrorState, IconButton, Loading, Shade } from './ui/primitives.tsx';
import { CreateChannel } from './forms/create-channel.tsx';
import { DraftProvider } from './forms/drafts.tsx';
import { MentionProvider } from './ui/mentions.tsx';

function useInitialCover(ready:boolean,epoch:number){
  const [covered,setCovered]=useState(true),started=useRef(performance.now());
  useEffect(()=>{started.current=performance.now();setCovered(true);},[epoch]);
  useEffect(()=>{
    if(!ready)return;let frame:number|null=null;
    const timer=setTimeout(()=>{frame=requestAnimationFrame(()=>{frame=requestAnimationFrame(()=>setCovered(false));});},Math.max(0,1000-(performance.now()-started.current)));
    return()=>{clearTimeout(timer);if(frame!==null)cancelAnimationFrame(frame);};
  },[ready,epoch]);return covered;
}
/** One application shell; route changes never recreate its Apollo or live owner. */
export function App(){
  const session=useSession(),[directory,setDirectory]=useState<string|null>(null),[mobile,setMobile]=useState(()=>matchMedia('(width < 768px)').matches);
  const [navigationOpen,setNavigationOpen]=useState(false),[creating,setCreating]=useState(false);
  const menu=useRef<HTMLButtonElement>(null),nav=useRef<HTMLElement>(null),main=useRef<HTMLDivElement>(null);
  const route=session.route,thread=!!route?.root,wasThread=useRef(false),previous=useRef<HTMLElement|null>(null);
  const covered=useInitialCover((!!route&&!session.routeBusy)||!!session.routeError,session.epoch);
  useEffect(()=>{
    const controller=new AbortController();void viewerDirectory(controller.signal).then(value=>{setDirectory(value);document.title=value?`AgentChat (${value})`:'AgentChat';}).catch(()=>{});
    return()=>controller.abort();
  },[]);
  useEffect(()=>{
    const query=matchMedia('(width < 768px)'),changed=()=>{setMobile(query.matches);setNavigationOpen(false);};
    query.addEventListener('change',changed);return()=>query.removeEventListener('change',changed);
  },[]);
  useEffect(()=>{setNavigationOpen(false);},[route?.channel]);
  useEffect(()=>{
    if(thread&&!wasThread.current){previous.current=document.activeElement as HTMLElement;requestAnimationFrame(()=>document.querySelector<HTMLButtonElement>('[aria-label="Close thread"]')?.focus({preventScroll:true}));}
    if(!thread&&wasThread.current){if(previous.current?.isConnected)previous.current.focus({preventScroll:true});else main.current?.focus({preventScroll:true});}
    wasThread.current=thread;
  },[thread]);
  useEffect(()=>{
    if(!mobile||!navigationOpen)return;
    const keyboard=(event:KeyboardEvent)=>{
      if(event.key==='Escape'){event.preventDefault();setNavigationOpen(false);menu.current?.focus();}
      if(event.key==='Tab'){
        const items=[menu.current,...(nav.current?.querySelectorAll<HTMLElement>('button,a[href]')??[])].filter((n):n is HTMLElement=>!!n);
        const index=items.indexOf(document.activeElement as HTMLElement),next=(index+(event.shiftKey?-1:1)+items.length)%items.length;
        event.preventDefault();items[next]?.focus();
      }
    };
    document.addEventListener('keydown',keyboard,true);return()=>document.removeEventListener('keydown',keyboard,true);
  },[mobile,navigationOpen]);
  useEffect(()=>{
    if(!thread || navigationOpen || creating)return;
    const keyboard=(event:KeyboardEvent)=>{
      if(event.defaultPrevented || document.querySelector('dialog[open],#mention-profile'))return;
      if(event.key==='Escape'){event.preventDefault();if(route)session.actions.go(route.channel);}
      if(event.key==='Tab'){
        const items=[...document.querySelectorAll<HTMLElement>('[data-testid="thread-panel"] button:not(:disabled),[data-testid="thread-panel"] a[href],[data-testid="thread-panel"] textarea,[data-testid="thread-panel"] [tabindex="0"]')];
        const first=items[0],last=items.at(-1);
        if(event.shiftKey&&document.activeElement===first){event.preventDefault();last?.focus();}
        else if(!event.shiftKey&&document.activeElement===last){event.preventDefault();first?.focus();}
      }
    };
    document.addEventListener('keydown',keyboard);return()=>document.removeEventListener('keydown',keyboard);
  },[thread,navigationOpen,creating,route,session.actions]);
  const closeThread=()=>{if(route)session.actions.go(route.channel);};
  return <DraftProvider><MentionProvider>
    {covered&&<Loading fullscreen/>}
    <div className="shell grid h-full grid-cols-[220px_minmax(0,1fr)] grid-rows-[auto_minmax(0,1fr)] max-md:grid-cols-[minmax(0,1fr)]" inert={covered} aria-hidden={covered}>
      <header className="bar col-span-2 flex h-14 items-center justify-between gap-4 border-b border-slate-200 bg-white px-6 max-md:relative max-md:z-40 max-md:col-span-full max-md:gap-2 max-md:px-3">
        <div className="flex min-w-0 items-center gap-3 max-md:gap-2"><IconButton ref={menu} size="lg" className="menu-button hidden! text-slate-600 max-md:inline-flex!" aria-label={navigationOpen?'Close navigation':'Open navigation'} aria-expanded={mobile&&navigationOpen} aria-controls="project-navigation" onClick={()=>setNavigationOpen(v=>!v)}><Menu size={20} aria-hidden="true"/></IconButton>
          <div className="heading flex min-w-0 items-center gap-6 max-md:gap-3"><h1 className="brand shrink-0 text-lg font-bold tracking-tight">AgentChat</h1>
            {directory&&<span className="directory inline-flex min-w-0 items-center gap-2 rounded-full bg-slate-100 px-3 py-1 text-sm text-slate-500 max-md:gap-1 max-md:overflow-hidden max-md:px-2" title={directory}><Folder size={14} className="shrink-0" aria-hidden="true"/><span className="truncate">{directory}</span></span>}
          </div>
        </div>
        <span className="status flex shrink-0 items-center gap-3 text-sm text-slate-400"><span aria-hidden="true" className={`dot size-[5px] rounded-full ${session.connection==='live'?'live bg-violet-500':'bg-violet-300'}`}/><span>{session.connection==='live'?'Live':session.connection==='hidden'?'Paused':session.connection==='connecting'?'Connecting…':'Connection paused'}</span></span>
      </header>
      <div hidden={!mobile||!navigationOpen} className="navigation-backdrop fixed inset-x-0 top-14 bottom-0 z-20"><Shade onDismiss={()=>{setNavigationOpen(false);menu.current?.focus();}} label="Dismiss navigation"/></div>
      <nav ref={nav} id="project-navigation" role="navigation" aria-label="Project navigation" inert={(thread&&!navigationOpen)||(mobile&&!navigationOpen)} aria-hidden={(thread&&!navigationOpen)||(mobile&&!navigationOpen)}
        className={`sidebar flex min-h-0 flex-col border-r border-slate-200 bg-white max-md:fixed max-md:top-14 max-md:bottom-0 max-md:left-0 max-md:z-30 max-md:w-[min(220px,100vw)] max-md:shadow-xl ${mobile&&!navigationOpen?'navigation-closed':'navigation-open'}`}>
        <Sidebar focusSelected={mobile&&navigationOpen} onNavigate={()=>setNavigationOpen(false)} onCreate={()=>{setNavigationOpen(false);setCreating(true);}}/>
      </nav>
      <main className="content flex min-h-0 min-w-0 flex-col" inert={mobile&&navigationOpen} aria-busy={session.routeBusy}>
        {session.notice&&<div className="banner flex shrink-0 items-center justify-between gap-3 border-b border-violet-100 bg-violet-50 px-6 py-2.5 text-sm text-violet-700"><ErrorState compact role="status" message={session.notice}/>
          {(session.connection==='reset'||session.connection==='error')&&<button className="shrink-0 font-semibold text-violet-600" onClick={session.actions.reset}>Reload snapshot</button>}
        </div>}
        {session.routeError?<ErrorState message={session.routeError}><button className="text-violet-600" onClick={session.actions.reset}>Retry</button></ErrorState>:route?
          <div className="relative grid min-h-0 flex-1 grid-cols-[minmax(0,1fr)] overflow-clip">
            <div ref={main} tabIndex={-1} inert={thread||session.routeBusy} className="root-pane z-0 min-h-0"><Panel channel={route.channel} root={null} covered={covered||creating||thread||session.routeBusy||(mobile&&navigationOpen)} selectedRoot={route.root}/></div>
            <div hidden={!thread} className="backdrop fixed inset-x-0 top-14 bottom-0 z-[5]"><Shade label="Close thread" onDismiss={closeThread}/></div>
            <div hidden={!thread} inert={!thread} aria-hidden={!thread} className="thread-panel absolute inset-y-0 right-0 z-10 w-[50vw] border-l border-slate-200 bg-slate-50 shadow-xl max-md:w-full">
              {route.root&&<Panel channel={route.channel} root={route.root} covered={covered||creating||(mobile&&navigationOpen)} onClose={closeThread}/>}
            </div>
          </div>:<Loading/>}
      </main>
    </div>
    <CreateChannel open={creating} onDismiss={()=>{setCreating(false);if(mobile)requestAnimationFrame(()=>menu.current?.focus());}}/>
  </MentionProvider></DraftProvider>;
}
