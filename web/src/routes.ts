/** Viewer locations select read-only views, never a project or participation. */
export interface ViewRoute { channel:string; root:string | null }
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
export function routePath(channel:string, root:string | null = null):string {
  return `/${encodeURIComponent(channel)}${root ? `/${root}` : ''}`;
}
export function parseRoute(path:string):ViewRoute | null {
  if (path === '/') return {channel:'',root:null};
  const parts = path.split('/');
  if (parts[0] !== '' || (parts.length !== 2 && parts.length !== 3)) return null;
  try {
    const channel = decodeURIComponent(parts[1]!);
    if (channel.length>64 || !/^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$/.test(channel)) return null;
    const root = parts[2] ?? null;
    return root === null || uuid.test(root) ? {channel,root} : null;
  } catch { return null; }
}
/** Modified clicks retain native open-in-new-tab/window behavior. */
export function plainClick(event:MouseEvent):boolean {
  return event.button === 0 && !event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey;
}
