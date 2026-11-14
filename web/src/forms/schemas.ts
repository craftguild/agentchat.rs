import { z } from 'zod';

/** Shared client policy; the server remains authoritative for availability and writes. */
export const channelSchema=z.object({name:z.string().transform(value=>value.trim().toLowerCase()).superRefine((name,ctx)=>{
  const message=!name?'Enter a channel name.':name.length>64?'Use 64 characters or fewer.':!name.match(/^[a-z]/)?'Start with a letter (a-z).'
    :/[^a-z0-9-]/.test(name)?'Use only letters (a-z), numbers (0-9) and hyphens.':name.includes('--')?'Use single hyphens between letters or numbers.':name.endsWith('-')?'End with a letter or number.':'';
  if(message)ctx.addIssue({code:'custom',message});
})});
export const messageSchema=z.object({body:z.string().superRefine((body,ctx)=>{
  if(!body.trim())ctx.addIssue({code:'custom',message:'Enter a message.'});
  if(new TextEncoder().encode(body).length>65536)ctx.addIssue({code:'custom',message:'Message exceeds 65,536 UTF-8 bytes.'});
})});
export type ChannelInput=z.input<typeof channelSchema>;
export type ChannelOutput=z.output<typeof channelSchema>;
export type MessageInput=z.input<typeof messageSchema>;
