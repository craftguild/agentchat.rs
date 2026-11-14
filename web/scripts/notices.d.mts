/** Build-only notice verification and emitted package inventory. */
export function loadNotices(root?: string): Promise<{ names: Set<string>; text: string }>;
export function npmPackage(path: string): string | undefined;
