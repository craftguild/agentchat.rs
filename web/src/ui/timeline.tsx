import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import { useSession } from '../state/session.tsx';
import { useSdk } from '../graphql/provider.tsx';
import type { Timeline as TimelineState, Anchor } from '../state/timeline.ts';
import { viewKey } from '../state/timeline.ts';
import { hydrateHistory } from '../state/history.ts';
import { sectionStart, unreadReplies } from '../state/reading.ts';
import { compare } from '../types.ts';
import { MessageCard } from './message.tsx';
import { ErrorState } from './primitives.tsx';

export interface TimelineProps {view:TimelineState;covered:boolean;selectedRoot:string|null}
/** Stable ID measurements and article anchors, independent of the virtualizer's indices. */
export function Timeline({view,covered,selectedRoot}:TimelineProps){
  const session=useSession(),{sdk,contents}=useSdk(),key=viewKey(view.channel,view.root);
  const position=session.actions.position(key),scroll=useRef<HTMLDivElement>(null);
  const rows=view.pinned?[view.pinned,...view.rows]:view.rows;
  const latest=useRef({view,covered,session,rows,key,position});latest.current={view,covered,session,rows,key,position};
  const restoring=useRef(false),movingUntil=useRef(0),direction=useRef(0),target=useRef(''),frame=useRef<number|null>(null);
  const [moving,setMoving]=useState(false),[hydrated,bump]=useState(0),[hydrationError,setHydrationError]=useState('');
  const settleTimer=useRef<ReturnType<typeof setTimeout>|null>(null),correctRef=useRef<()=>void>(()=>{}),correctionFrame=useRef<number|null>(null);
  // Resize delivery may measure more nodes after this callback. Keep anchor writes
  // outside that delivery cycle and coalesce row/viewport/DOM notifications.
  const scheduleCorrection=useCallback(()=>{
    if(correctionFrame.current!==null)return;
    correctionFrame.current=requestAnimationFrame(()=>{correctionFrame.current=null;correctRef.current();});
  },[]);
  const getItemKey=useCallback((index:number)=>rows[index]!.message.id,[rows.map(r=>r.message.id).join(',')]);
  const virtualizer=useVirtualizer({count:rows.length,getScrollElement:()=>scroll.current,getItemKey,
    // Row measurement may rerender the observed subtree; defer that cycle too.
    useAnimationFrameWithResizeObserver:true,
    estimateSize:index=>position.heights.get(rows[index]!.message.id)??120,overscan:3,paddingStart:16,
    measureElement:(element,entry)=>{
      const height=entry?.borderBoxSize?.[0]?.blockSize ?? element.getBoundingClientRect().height;
      if(element instanceof HTMLElement && element.dataset.id && height>0)position.heights.set(element.dataset.id,height);
      scheduleCorrection();return height;
    },
  });
  virtualizer.shouldAdjustScrollPositionOnItemSizeChange=()=>false;
  const capture=useCallback(():Anchor|null=>{
    const list=scroll.current;if(!list)return null;const top=list.getBoundingClientRect().top;
    const row=[...list.querySelectorAll<HTMLElement>('[data-id]')].find(row=>(row.querySelector('article')??row).getBoundingClientRect().bottom>top+1);
    return row ? {id:row.dataset.id!,offset:(row.querySelector('article')??row).getBoundingClientRect().top-top} : null;
  },[]);
  const correct=useCallback(()=>{
    const list=scroll.current,{view,position}=latest.current;
    if(!list || !list.clientHeight || view.target || performance.now()<movingUntil.current || !position.anchor)return;
    const row=list.querySelector<HTMLElement>(`[data-id="${position.anchor.id}"]`);
    if(!row)return;
    const delta=(row.querySelector('article')??row).getBoundingClientRect().top-list.getBoundingClientRect().top-position.anchor.offset;
    if(Math.abs(delta)>.5){restoring.current=true;list.scrollTop+=delta;requestAnimationFrame(()=>{restoring.current=false;});}
  },[]);
  correctRef.current=correct;
  useLayoutEffect(()=>{
    const retained=new Set(rows.map(row=>row.message.id));for(const id of position.heights.keys())if(!retained.has(id))position.heights.delete(id);
    correct();
  });
  useEffect(()=>{
    const list=scroll.current;if(!list)return;
    const mutations=new MutationObserver(scheduleCorrection),resize=new ResizeObserver(scheduleCorrection);
    mutations.observe(list,{childList:true,subtree:true,attributes:true});resize.observe(list);
    return()=>{mutations.disconnect();resize.disconnect();if(correctionFrame.current!==null){cancelAnimationFrame(correctionFrame.current);correctionFrame.current=null;}};
  },[key,scheduleCorrection]);
  useLayoutEffect(()=>{
    const intent=view.target;if(!intent)return;
    const identity=`${key}:${intent.id}:${intent.version}`;
    if(target.current===identity)return;target.current=identity;restoring.current=true;
    let stable=0,cancelled=false,attempts=0;
    const land=()=>{
      if(cancelled)return;const list=scroll.current,current=latest.current;
      const index=current.rows.findIndex(row=>row.message.id===intent.id);
      if(!list || index<0 || !list.clientHeight){frame.current=requestAnimationFrame(land);return;}
      const row=list.querySelector<HTMLElement>(`[data-id="${intent.id}"]`),card=row?.querySelector('article');
      if(card){
        const delta=card.getBoundingClientRect().bottom-list.getBoundingClientRect().bottom+parseFloat(getComputedStyle(row!).paddingBottom);
        if(Math.abs(delta)>1){list.scrollTop+=delta;stable=0;}else stable++;
        if((stable>=2 && row!.hasAttribute('data-full')) || ++attempts>30){
          current.position.anchor=capture();restoring.current=false;
          current.session.actions.completeTarget(current.key,intent.version);return;
        }
      }else virtualizer.scrollToIndex(index,{align:'end'});
      frame.current=requestAnimationFrame(land);
    };
    frame.current=requestAnimationFrame(land);
    return()=>{cancelled=true;if(target.current===identity)target.current='';if(frame.current!==null)cancelAnimationFrame(frame.current);restoring.current=false;};
  },[key,view.target?.id,view.target?.version,virtualizer,capture]);
  useLayoutEffect(()=>{
    if(view.target || !position.anchor)return;
    const index=rows.findIndex(row=>row.message.id===position.anchor!.id);
    if(index>=0 && !scroll.current?.querySelector(`[data-id="${position.anchor.id}"]`))virtualizer.scrollToIndex(index,{align:'start'});
    correct();
  },[key,view.revision,correct,virtualizer]);
  const items=virtualizer.getVirtualItems(),range=items.map(item=>item.key).join(',');
  useEffect(()=>{
    const abort=new AbortController();setHydrationError('');
    const wanted=items.slice(0,32).map(item=>rows[item.index]!).filter(row=>!contents.has(row.message.id));
    if(wanted.length)void hydrateHistory(sdk,contents,view,wanted,abort.signal).then(()=>{if(!abort.signal.aborted)bump(n=>n+1);})
      .catch(error=>{if(!abort.signal.aborted){session.actions.fail(error);setHydrationError((error as Error).message);}});
    return()=>abort.abort();
  },[key,range,view.revision,sdk,contents,hydrated]);
  useEffect(()=>{
    const list=scroll.current;if(!list || covered || moving || view.target || document.visibilityState!=='visible')return;
    const mark=()=>{
      const current=latest.current;if(current.covered || restoring.current || performance.now()<movingUntil.current || document.visibilityState!=='visible')return;
      const viewport=list.getBoundingClientRect(),reading=current.session.reading;
      const posts=current.rows.filter(row=>{
        if(reading?.seen.has(row.message.id) && !reading.pending.has(row.message.id))return false;
        const card=list.querySelector(`[data-id="${row.message.id}"][data-full] article`),box=card?.getBoundingClientRect();
        return box && Math.min(box.bottom,viewport.bottom)>Math.max(box.top,viewport.top) && Math.min(box.right,viewport.right)>Math.max(box.left,viewport.left);
      }).map(row=>row.message);
      if(posts.length)current.session.actions.mark(posts);
    };
    const observer=new IntersectionObserver(mark,{root:list,threshold:[0,.01]});
    for(const article of list.querySelectorAll('[data-full] article'))observer.observe(article);
    mark();document.addEventListener('visibilitychange',mark);
    return()=>{observer.disconnect();document.removeEventListener('visibilitychange',mark);};
  },[key,range,hydrated,covered,moving,view.target,view.revision]);
  const interrupt=(delta:number)=>{
    movingUntil.current=performance.now()+250;direction.current=Math.sign(delta);setMoving(true);
    latest.current.session.actions.cancelTarget(key);restoring.current=false;
    if(settleTimer.current!==null)clearTimeout(settleTimer.current);
    settleTimer.current=setTimeout(()=>{position.anchor=capture();setMoving(false);},270);
  };
  useEffect(()=>()=>{if(settleTimer.current!==null)clearTimeout(settleTimer.current);if(frame.current!==null)cancelAnimationFrame(frame.current);},[]);
  const boundary=session.reading?sectionStart(session.reading,view.rows[0]?.message.channelId??view.pinned?.message.channelId??'',view.root):null;
  const boundaryId=boundary?view.rows.find(row=>compare(row.message.creationSeq,boundary.creationSeq)>=0)?.message.id:null;
  return <>
    {hydrationError && <div className="px-5 py-2"><ErrorState compact message={hydrationError}><button className="text-violet-600" onClick={()=>bump(n=>n+1)}>Retry</button></ErrorState></div>}
    <div ref={scroll} role="log" aria-label={view.root?'Thread messages':'Channel messages'} tabIndex={0} data-testid="message-list" data-total={rows.length}
      className="list min-h-0 min-w-0 flex-1 overflow-auto overscroll-contain [overflow-anchor:none]"
      onWheel={event=>interrupt(event.deltaY)} onTouchStart={()=>interrupt(-1)} onTouchMove={()=>interrupt(-1)} onPointerDown={()=>interrupt(0)}
      onKeyDown={event=>{if(['Home','PageUp','ArrowUp'].includes(event.key))interrupt(-1);if(['End','PageDown','ArrowDown'].includes(event.key)){interrupt(1);if(event.key==='End'&&view.newerGap)void session.actions.load(view.channel,view.root);}}}
      onScroll={()=>{
        if(performance.now()<movingUntil.current){position.anchor=capture();if(direction.current<0 && scroll.current!.scrollTop<150 && view.before&&!view.busy)void session.actions.load(view.channel,view.root,true);}
      }}>
      <div className="relative w-full" style={{height:virtualizer.getTotalSize()}}>
        {items.map(item=>{
          const row=rows[item.index]!;if(!row)return null;
          const body=contents.get(row.message.id),pinned=row===view.pinned,separator=pinned&&!!view.rows.length;
          const newSection=row.message.id===boundaryId;
          return <div key={row.message.id} ref={virtualizer.measureElement} data-index={item.index} data-id={row.message.id}
            data-full={body?'':undefined} data-pinned={pinned?'':undefined} data-thread-separator={separator?'':undefined}
            className={`frame absolute left-0 top-0 w-full px-[var(--chat-card-gutter)] pb-4 max-[700px]:pb-3 ${body?'':'overflow-hidden'}`}
            style={{transform:`translateY(${item.start}px)`,...(!body?{height:position.heights.get(row.message.id)??120}:{})}}>
            {newSection&&<div role="heading" aria-level={3} className="new-section mb-4 flex items-center gap-3 text-sm font-semibold text-violet-600"><span className="h-px flex-1 bg-violet-200"/>New Messages<span className="h-px flex-1 bg-violet-200"/></div>}
            <MessageCard row={row} body={body} pinned={pinned} agents={session.directory.agents} selected={!view.root&&row.message.id===selectedRoot}
              unreadReplies={session.reading?unreadReplies(session.reading,row.message.id):false}/>
            {separator&&<div className="mt-4 border-t border-slate-200 max-[700px]:mt-3" style={{marginInline:'calc(-1 * var(--chat-card-gutter))'}}/>}
          </div>;
        })}
      </div>
    </div>
  </>;
}
