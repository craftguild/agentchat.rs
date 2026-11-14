/** Static build metadata is frozen into the binary, never read from runtime Git. */
export function buildLabel(value:unknown): string {
  if (!value || typeof value !== 'object') throw new Error('Invalid build metadata.');
  const {version,revision} = value as Record<string,unknown>;
  if (typeof version !== 'string' || version.length > 64 || !/^\d+\.\d+\.\d+(?:[-+][\w.+-]+)?$/.test(version)
    || typeof revision !== 'string' || !/^(?:[0-9a-f]{40}|[0-9a-f]{64}|unknown)$/.test(revision)) {
    throw new Error('Invalid build metadata.');
  }
  return `v${version} (${revision.slice(0, 7)})`;
}
