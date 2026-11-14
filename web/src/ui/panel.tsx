import { X } from 'lucide-react';
import { useSession } from '../state/session.tsx';
import { viewKey } from '../state/timeline.ts';
import { newestPost } from '../state/reading.ts';
import { Timeline } from './timeline.tsx';
import { IconButton, Loading, ErrorState } from './primitives.tsx';
import { Composer } from '../forms/composer.tsx';

export interface PanelProps {channel:string;root:string|null;covered:boolean;onClose?:()=>void;selectedRoot?:string|null}
/** Independent main/thread viewport, sharing the same composer and card gutters. */
export function Panel({channel,root,covered,onClose,selectedRoot=null}:PanelProps){
  const session=useSession(),view=session.views.get(viewKey(channel,root));
  const members=session.directory.members,handles=session.directory.agents.filter(a=>a.joined&&members?.ids.has(a.id)).map(a=>a.handle);
  const channelId=session.directory.channels.find(c=>c.name===channel)?.id??'';
  const newest=session.reading?newestPost(session.reading,channelId):null;
  const jump=()=>{if(!newest)return;session.actions.go(channel,newest.rootId);session.actions.reveal(channel,newest.rootId,newest.id);void session.actions.load(channel,newest.rootId);};
  return <section className="chat-pane relative flex h-full min-h-0 min-w-0 flex-col bg-slate-50" role="region" aria-label={root?'Thread':`Channel ${channel}`} data-testid={root?'thread-panel':'main-panel'}>
    <header className="flex h-16 shrink-0 items-center justify-between border-b border-slate-200 bg-white px-6 max-[700px]:px-4">
      <h2 className="flex min-w-0 items-center text-sm font-bold">{root?'Thread':<span className="channel-name truncate"># {channel}</span>}
        {root?<span className="subtitle ml-2.5 text-sm font-normal text-slate-400">{view?.pinned?.message.replyCount??'0'} {view?.pinned?.message.replyCount==='1'?'reply':'replies'}</span>
          :<span className="channel-members ml-2.5 min-w-[8ch] shrink-0 text-sm font-normal text-slate-400 tabular-nums" aria-busy={session.directory.membersBusy} title={members?'Channel participants':'Channel membership unknown'}>
            {members===null?'—':members.count} {members?.count===1?'agent':'agents'}
          </span>}
      </h2>
      {root&&<IconButton aria-label="Close thread" onClick={onClose}><X size={18} aria-hidden="true"/></IconButton>}
    </header>
    <div className="history relative flex min-h-0 flex-1 flex-col" aria-busy={!!view?.busy}>
      {view?.error && (view.rows.length||view.pinned) ? <div className="px-5 py-2"><ErrorState compact role="status" message={view.error}><button className="text-violet-600" onClick={()=>void session.actions.load(channel,root,view.lastLoad==='older')}>Retry</button></ErrorState></div>:null}
      {!view || (view.busy&&!view.rows.length&&!view.pinned)?<Loading/>:!view.rows.length&&!view.pinned?
        view.error?<ErrorState message={view.error}><button className="text-violet-600" onClick={()=>void session.actions.load(channel,root)}>Retry</button></ErrorState>
          :<div role="status" className="grid flex-1 place-content-center gap-2.5 p-9 text-center text-sm text-slate-400"><b className="text-slate-500">{root?'No replies yet':'A little quiet here'}</b><span>{root?'Replies will appear here as they arrive.':'Messages will appear here as they arrive.'}</span></div>
        :<Timeline view={view} covered={covered} selectedRoot={selectedRoot}/>}
      {newest&&<button className="latest absolute bottom-4 left-1/2 z-[6] -translate-x-1/2 whitespace-nowrap rounded-full border border-slate-200 bg-white px-4 py-2 text-sm font-semibold text-violet-600 shadow-md" onClick={jump}>Newest Message ↓</button>}
    </div>
    <Composer key={viewKey(channel,root)} channel={channel} root={root} ready={!!view&&!view.busy&&!view.error} handles={handles}/>
  </section>;
}
