import { useEffect, useRef } from 'react';
import type { ComponentPropsWithRef, ReactNode } from 'react';
import { Unlink, X } from 'lucide-react';

/** Shared controls keep variants explicit without selector aliases. */
export function Button({variant='secondary',className='',...props}:ComponentPropsWithRef<'button'> & {variant?:'primary'|'secondary'}) {
  return <button {...props} className={`rounded-md px-3 py-2 text-sm font-medium transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${variant==='primary'?'bg-violet-600 text-white hover:bg-violet-700':'text-slate-500 hover:bg-slate-100'} ${className}`}/>;
}
export function IconButton({className='',size='md',...props}:ComponentPropsWithRef<'button'> & {size?:'sm'|'md'|'lg'}){
  return <button type="button" {...props} className={`inline-flex shrink-0 items-center justify-center rounded-md text-slate-400 hover:bg-violet-50 hover:text-violet-600 disabled:opacity-50 ${size==='sm'?'size-7':size==='lg'?'size-9':'size-8'} ${className}`}/>;
}
export function ErrorState({message,compact=false,children,role='alert'}:{message:string;compact?:boolean;children?:ReactNode;role?:'alert'|'status'}){
  return <div role={role} className={compact?'flex items-center gap-2 text-sm text-slate-400':'flex min-h-0 flex-1 items-center justify-center p-8'}>
    <div className={`flex items-center text-slate-400 text-sm ${compact?'gap-2':'flex-col text-center gap-3'}`}>
      <Unlink size={compact?16:32} className="shrink-0" aria-hidden="true"/><span>{message.replace(/\.$/,'')}</span>{children}
    </div></div>;
}
export function Loading({fullscreen=false}:{fullscreen?:boolean}){
  return <div className={fullscreen?'fixed inset-0 z-[1000] grid place-content-center bg-slate-50':'grid min-h-24 flex-1 place-content-center'}>
    <span role="status" aria-label="Loading conversations" className="size-7 rounded-full border-[3px] border-violet-200 border-t-violet-600 animate-spin [animation-duration:.8s] motion-reduce:animate-none"/>
  </div>;
}
export function Shade({onDismiss,label='Dismiss overlay',className=''}:{onDismiss:()=>void;label?:string;className?:string}){
  return <button type="button" tabIndex={-1} aria-label={label} onClick={onDismiss} className={`absolute inset-0 size-full border-0 bg-[#0f172a]/20 p-0 cursor-default ${className}`}/>;
}
/** Native top-layer dialog supplies inertness and keyboard containment. */
export function Dialog({open,title,busy=false,onDismiss,children}:{open:boolean;title:string;busy?:boolean;onDismiss:()=>void;children:ReactNode}){
  const ref=useRef<HTMLDialogElement>(null),previous=useRef<HTMLElement|null>(null);
  useEffect(()=>{
    const dialog=ref.current!;
    if(open && !dialog.open){previous.current=document.activeElement as HTMLElement;dialog.showModal();dialog.querySelector<HTMLElement>('[data-autofocus]')?.focus();}
    else if(!open && dialog.open){dialog.close();if(previous.current?.isConnected)previous.current.focus({preventScroll:true});}
  },[open]);
  useEffect(()=>()=>ref.current?.close(),[]);
  return <dialog ref={ref} aria-labelledby="dialog-heading" onCancel={event=>{event.preventDefault();if(!busy)onDismiss();}}
    className="fixed inset-0 m-0 size-full max-h-none max-w-none border-0 bg-transparent p-4 open:grid open:place-items-center backdrop:bg-transparent">
    <Shade label="Close dialog" onDismiss={()=>{if(!busy)onDismiss();}}/>
    <section className="relative z-10 w-full max-w-md rounded-xl bg-white p-6 text-slate-700 shadow-xl">
      <header className="mb-5 flex items-center justify-between gap-4"><h2 id="dialog-heading" className="text-sm font-bold">{title}</h2>
        <IconButton aria-label="Close dialog" disabled={busy} onClick={onDismiss}><X size={18} aria-hidden="true"/></IconButton></header>
      {children}
    </section>
  </dialog>;
}
