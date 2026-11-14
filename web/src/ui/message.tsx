import { Link } from 'react-router';
import { MessageSquareReply } from 'lucide-react';
import type { Agent } from '../types.ts';
import type { Row } from '../state/timeline.ts';
import type { Body } from '../graphql/client.ts';
import { routePath } from '../routes.ts';
import { Identity } from './identity.tsx';
import { MessageBody } from './mentions.tsx';

const timestamp=new Intl.DateTimeFormat(undefined,{calendar:'gregory',year:'numeric',month:'2-digit',day:'2-digit',hour:'2-digit',minute:'2-digit'});
export interface MessageCardProps {
  row:Row;body:Body|undefined;agents:readonly Agent[];pinned?:boolean;selected?:boolean;unreadReplies?:boolean;
}
/** Card geometry is natural; only the containing timeline scrolls vertically. */
export function MessageCard({row,body,agents,pinned=false,selected=false,unreadReplies=false}:MessageCardProps){
  const message=row.message,date=new Date(message.createdAt),host=agents.find(a=>a.id===message.author.id)?.host ?? '';
  return <article aria-label={`Message from ${message.author.handle}`} className={`flex min-h-[88px] flex-col gap-3 rounded-md border-0 p-[18px] shadow-sm max-[700px]:gap-2.5 max-[700px]:p-3.5 ${selected?'bg-violet-50':'bg-white'}`}>
    <div className="message-heading flex min-w-0 items-start justify-between gap-2">
      <Identity author={message.author} host={host}><time dateTime={message.createdAt} title={message.createdAt}>{Number.isNaN(date.getTime())?message.createdAt:timestamp.format(date)}</time></Identity>
      {message.rootId===null&&!pinned && <Link aria-label="Reply in thread" title="Reply in thread" data-open-root={message.id} to={routePath(message.channel,message.id)}
        className="flex size-8 shrink-0 items-center justify-center rounded-md text-slate-400 no-underline hover:bg-violet-50 hover:text-violet-600"><MessageSquareReply size={16} aria-hidden="true"/></Link>}
    </div>
    {body?<MessageBody html={body.bodyHtml} agents={agents}/>:<div className="min-h-5 text-sm text-slate-400">Loading message…</div>}
    {message.rootId===null&&!pinned&&message.replyCount!=='0' && <Link to={routePath(message.channel,message.id)}
      aria-label={`View thread (${new Intl.NumberFormat().format(BigInt(message.replyCount))})${unreadReplies?', unread replies':''}`}
      className="thread flex w-full shrink-0 items-center gap-2 rounded-md px-3 py-2.5 text-left text-sm font-semibold text-violet-600 no-underline hover:bg-violet-100/70 focus-visible:bg-violet-100/70">
      <span>View thread</span><span className="reply-count rounded-full bg-violet-100 px-2 py-0.5 text-xs tabular-nums">{new Intl.NumberFormat().format(BigInt(message.replyCount))}</span>
      {unreadReplies&&<span className="unread-dot size-2 rounded-full bg-violet-500" aria-hidden="true"/>}
    </Link>}
  </article>;
}
