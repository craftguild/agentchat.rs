import { useLayoutEffect, useRef, useState } from 'react';
import type { KeyboardEvent } from 'react';
import { useForm, useWatch } from 'react-hook-form';
import { zodResolver } from '@hookform/resolvers/zod';
import { Send } from 'lucide-react';
import { mentionOptions, mentionToken } from '../mention-completion.ts';
import { messageSchema } from './schemas.ts';
import type { MessageInput } from './schemas.ts';
import { useDraft } from './drafts.tsx';

export interface ComposerProps {channel:string;root:string|null;ready:boolean;handles:readonly string[]}
/** One RHF instance per destination; pending writes belong to the shared draft owner. */
export function Composer({channel,root,ready,handles}:ComposerProps){
  const {draft,edit,send}=useDraft(channel,root);
  const form=useForm<MessageInput>({resolver:zodResolver(messageSchema),mode:'onChange',defaultValues:{body:draft.text}});
  const body=useWatch({control:form.control,name:'body'}),[caret,setCaret]=useState(0),[dismissed,setDismissed]=useState(false),[choice,setChoice]=useState(0);
  const composing=useRef(false),ended=useRef(-Infinity),editor=useRef<HTMLTextAreaElement|null>(null);
  const token=mentionToken(body,caret),options=token && !dismissed && !composing.current && !draft.pending ? mentionOptions(token.query,handles) : [];
  const field=form.register('body'),valid=messageSchema.safeParse({body});
  useLayoutEffect(()=>{const input=editor.current;if(input){input.style.height='auto';input.style.height=`${Math.min(input.scrollHeight,160)}px`;}},[body]);
  const select=(handle:string)=>{
    const current=mentionToken(body,caret);if(!current)return;
    const text=body.slice(0,current.start)+`@${handle} `+body.slice(current.end),position=current.start+handle.length+2;
    form.setValue('body',text,{shouldDirty:true,shouldValidate:true});edit(text);setCaret(position);setDismissed(true);
    requestAnimationFrame(()=>{editor.current?.focus();editor.current?.setSelectionRange(position,position);});
  };
  const submit=form.handleSubmit(async({body})=>{
    if(!ready || draft.pending || composing.current)return;
    setDismissed(true);if(await send(body))form.reset({body:''});
  });
  const key=(event:KeyboardEvent<HTMLTextAreaElement>)=>{
    if(event.nativeEvent.isComposing || composing.current || event.nativeEvent.keyCode===229 || performance.now()-ended.current<30)return;
    if(options.length){
      if(event.key==='Escape'){event.preventDefault();event.stopPropagation();setDismissed(true);return;}
      if(event.key==='ArrowDown'||event.key==='ArrowUp'){event.preventDefault();setChoice((choice+(event.key==='ArrowDown'?1:-1)+options.length)%options.length);return;}
      if((event.key==='Enter'&&!event.shiftKey)||event.key==='Tab'){event.preventDefault();select(options[choice%options.length]!);return;}
    }
    if(event.key==='Enter'&&!event.shiftKey){event.preventDefault();void submit();}
  };
  return <form onSubmit={submit} className="shrink-0 border-t border-slate-200 bg-slate-50 py-4 px-[var(--chat-card-gutter)]">
    <div className="editor relative rounded-md border-0 bg-white shadow-sm focus-within:shadow-md">
      {!!options.length && <div id={`mentions-${root??'main'}`} role="listbox" aria-label="Mention recipients" className="absolute bottom-full left-0 z-20 mb-2 max-h-60 w-full max-w-sm overflow-auto rounded-lg border border-slate-200 bg-white p-1 shadow-lg">
        {options.map((handle,index)=><button type="button" role="option" key={handle} id={`mention-${root??'main'}-${index}`} aria-selected={index===choice%options.length} tabIndex={-1}
          onMouseDown={event=>event.preventDefault()} onClick={()=>select(handle)}
          className="flex w-full items-center justify-between gap-4 rounded-md px-3 py-2 text-left text-sm text-slate-700 hover:bg-violet-50 aria-selected:bg-violet-50 aria-selected:text-violet-700">
          @{handle}<span className="text-xs text-slate-400">{['all','here','channel'].includes(handle)?handle==='here'?'Connected channel agents':'All channel agents':'Channel agent'}</span>
        </button>)}
      </div>}
      <textarea {...field} ref={node=>{field.ref(node);editor.current=node;}} rows={2} maxLength={65536}
        aria-label={root?'Reply to thread':`Message #${channel}`} placeholder={root?'Reply as user…':`Message #${channel} as user…`}
        disabled={draft.pending||!ready} aria-autocomplete="list" aria-controls={options.length?`mentions-${root??'main'}`:undefined}
        aria-expanded={!!options.length} aria-activedescendant={options.length?`mention-${root??'main'}-${choice%options.length}`:undefined}
        onChange={event=>{void field.onChange(event);setCaret(event.target.selectionStart);setChoice(0);setDismissed(false);form.clearErrors('root');try{edit(event.target.value);}catch(error){form.setError('root',{message:(error as Error).message});}}}
        onClick={event=>{setCaret(event.currentTarget.selectionStart);setDismissed(false);}}
        onKeyUp={event=>{if(['ArrowLeft','ArrowRight','Home','End'].includes(event.key)){setCaret(event.currentTarget.selectionStart);setDismissed(false);}}}
        onBlur={event=>{void field.onBlur(event);setDismissed(true);}}
        onCompositionStart={()=>{composing.current=true;setDismissed(true);}}
        onCompositionEnd={event=>{composing.current=false;ended.current=performance.now();setCaret(event.currentTarget.selectionStart);setDismissed(false);}}
        onKeyDown={key} className="block max-h-40 min-h-16 w-full resize-none bg-transparent px-3 pt-3 pb-1 text-sm text-slate-700 outline-none disabled:opacity-60"/>
      <footer className="flex items-center justify-between gap-3 px-2 pb-2">
        <span className={`pl-1 text-xs max-[700px]:text-[10px] ${body && !valid.success?'text-red-600':'text-slate-400'}`}>
          {body && !valid.success?valid.error.issues[0]?.message:!ready?'Waiting for the conversation…':'Enter to send · Shift+Enter for a new line'}
        </span>
        <button type="submit" aria-label={root?'Send reply':'Send message'} title="Send" disabled={!ready||draft.pending||!valid.success||!!form.formState.errors.root}
          className="flex h-8 min-w-8 items-center justify-center rounded-md bg-violet-600 px-2 text-xs text-white hover:bg-violet-700 disabled:cursor-not-allowed disabled:bg-slate-200 disabled:text-slate-400">
          {draft.pending?'Sending…':<Send size={16} aria-hidden="true"/>}
        </button>
      </footer>
    </div>
    {(draft.error||form.formState.errors.root) && <p role="alert" className="mt-2 text-xs text-red-600">{draft.error||form.formState.errors.root?.message}</p>}
    {draft.warning && <p role="status" className="mt-2 text-xs text-slate-500">{draft.warning}</p>}
  </form>;
}
