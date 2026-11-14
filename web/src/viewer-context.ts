/** The display name is runtime metadata, never a project-routing capability. */
export async function viewerDirectory(signal:AbortSignal):Promise<string | null> {
  const response = await fetch('/viewer-context.json',{signal,cache:'no-store'});
  if (!response.ok) throw new Error('Viewer context unavailable.');
  return directoryLabel(await response.json());
}

/** Accept a directory name as plain text, never as HTML or a navigation URL. */
export function directoryLabel(value:unknown):string | null {
  if (!value || typeof value !== 'object') throw new Error('Invalid viewer context.');
  const {directoryName} = value as Record<string,unknown>;
  if (directoryName === null) return null;
  if (typeof directoryName !== 'string' || !directoryName.length || directoryName.length > 8192) throw new Error('Invalid viewer directory name.');
  return directoryName;
}
