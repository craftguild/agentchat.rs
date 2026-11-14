import { useRef, useState } from 'react';
import { useForm, useWatch } from 'react-hook-form';
import { zodResolver } from '@hookform/resolvers/zod';
import { CircleCheck, CircleAlert } from 'lucide-react';
import { useSdk } from '../graphql/provider.tsx';
import { useSession } from '../state/session.tsx';
import { Dialog, Button } from '../ui/primitives.tsx';
import { channelSchema } from './schemas.ts';
import type { ChannelInput, ChannelOutput } from './schemas.ts';

/** Live format feedback shares the exact submit schema and retry intent. */
export function CreateChannel({open,onDismiss}:{open:boolean;onDismiss:()=>void}){
  const {sdk}=useSdk(),session=useSession(),[busy,setBusy]=useState(false);
  const attempt=useRef<{name:string;id:string}|null>(null);
  const form=useForm<ChannelInput,unknown,ChannelOutput>({resolver:zodResolver(channelSchema),mode:'onChange',defaultValues:{name:''}});
  const name=useWatch({control:form.control,name:'name'}),parsed=channelSchema.safeParse({name});
  const state=!name?'':parsed.success?'valid':'invalid';
  const dismiss=()=>{if(busy)return;form.reset();attempt.current=null;onDismiss();};
  const submit=form.handleSubmit(async({name})=>{
    if(busy)return;
    if(attempt.current?.name!==name)attempt.current={name,id:crypto.randomUUID()};
    setBusy(true);form.clearErrors('root');
    try{const {createChannel:result}=await sdk.Create({requestId:attempt.current.id,name},{signal:new AbortController().signal});
      await session.directory.refreshChannels();form.reset();attempt.current=null;onDismiss();session.actions.go(result.name);
    }catch(error){form.setError('root',{message:(error as Error).message});}finally{setBusy(false);}
  });
  return <Dialog open={open} title="Create channel" busy={busy} onDismiss={dismiss}>
    <form onSubmit={submit}>
      <label htmlFor="channel-name" className="mb-2 block text-sm font-medium">Channel name</label>
      <div className="relative"><input id="channel-name" data-autofocus autoFocus autoComplete="off" spellCheck={false} placeholder="e.g. design" disabled={busy}
        {...form.register('name')} aria-invalid={state==='invalid'} aria-describedby="channel-name-rules channel-name-status"
        className={`block w-full rounded-md border py-2 pl-3 pr-10 text-sm outline-none focus:ring-2 ${state==='valid'?'border-emerald-500 focus:border-emerald-500 focus:ring-emerald-100':state==='invalid'?'border-red-400 focus:border-red-500 focus:ring-red-100':'border-slate-300 focus:border-violet-400 focus:ring-violet-100'}`}/>
        <span className={`field-state pointer-events-none absolute inset-y-0 right-3 flex items-center ${state==='valid'?'text-emerald-700':'text-red-600'}`} aria-hidden="true">
          {state && (parsed.success?<CircleCheck size={16}/>:<CircleAlert size={16}/>)}
        </span>
      </div>
      <p id="channel-name-rules" className="mt-2 text-xs leading-relaxed text-slate-500">1–64 characters. Start with a letter (a-z); use lowercase letters, numbers (0-9) and single hyphens. End with a letter or number.</p>
      <p id="channel-name-status" role="status" aria-live="polite" className="sr-only">{state && (parsed.success?'Valid channel name.':parsed.error.issues[0]?.message)}</p>
      {form.formState.errors.root && <p role="alert" className="mt-3 text-sm text-red-600">{form.formState.errors.root.message}</p>}
      <footer className="mt-5 flex justify-end gap-2"><Button type="button" disabled={busy} onClick={dismiss}>Cancel</Button><Button variant="primary" type="submit" disabled={busy||!parsed.success}>{busy?'Creating…':'Create'}</Button></footer>
    </form>
  </Dialog>;
}
