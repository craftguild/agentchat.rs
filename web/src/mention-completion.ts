/** Cursor-local input assistance; Core alone resolves actual recipients. */
export interface MentionToken { start:number; end:number; query:string }
export function mentionToken(value:string, caret:number):MentionToken | null {
  const prefix = value.slice(0,caret), match = /(?:^|[\s([{,:;!?])@([a-z0-9-]*)$/i.exec(prefix);
  if (!match) return null;
  const start = caret - match[1]!.length - 1;
  const tail = /^[a-z0-9-]*/i.exec(value.slice(caret))![0];
  return {start,end:caret + tail.length,query:match[1]!.toLowerCase()};
}
export function mentionOptions(query:string, handles:readonly string[]):string[] {
  return [...new Set(['all','here','channel',...handles.slice().sort()])].filter(h => h.toLowerCase().startsWith(query));
}
