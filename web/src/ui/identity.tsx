import { useMemo } from 'react';
import type { ReactNode } from 'react';
import { toSvg } from 'jdenticon';
import type { Author } from '../types.ts';

export interface IdentityProps {author:Author;host?:string;compact?:boolean;children?:ReactNode}
/** Identical author presentation in cards and retained-agent profiles. */
export function Identity({author,host='',compact=false,children}:IdentityProps){
  const svg=useMemo(()=>toSvg(author.handle,compact?28:36,{padding:.08}),[author.handle,compact]);
  return <div className={`flex min-w-0 items-center text-sm ${compact?'gap-2.5':'gap-3'}`}>
    <div className={`avatar shrink-0 overflow-hidden rounded [&_svg]:block [&_svg]:size-full ${compact?'size-7':'size-9'}`} aria-hidden="true" dangerouslySetInnerHTML={{__html:svg}}/>
    <div className="flex min-w-0 flex-1 items-center gap-2 overflow-hidden whitespace-nowrap">
      <span className="name min-w-0 truncate font-bold" title={author.handle}>{author.handle}</span>
      {host && <><span className="shrink-0 text-slate-400" aria-hidden="true">･</span><span className="host shrink-0 text-slate-400">{host}</span></>}
      {children && <><span className="shrink-0 text-slate-400" aria-hidden="true">･</span><span className="min-w-0 truncate text-slate-400">{children}</span></>}
    </div>
  </div>;
}
