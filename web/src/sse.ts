/** UTF-8 and SSE framing are independent of Fetch's arbitrary chunk boundaries. */
import { WireError } from './types.ts';

export const MAX_FRAME_BYTES = 1024 * 1024;
export interface SseEvent { event: 'next' | 'complete'; data: unknown }
interface Line { pieces: string[]; bytes: number }

function append(line: Line, value: string, frameBytes: number): void {
  if (!value) return;
  line.bytes += new TextEncoder().encode(value).length;
  if (line.bytes + frameBytes > MAX_FRAME_BYTES) throw overflow();
  // Merge similarly sized pieces, avoiding quadratic concatenation/scanning
  // when a complete large JSON line arrives a few bytes at a time.
  while (line.pieces.length && line.pieces.at(-1)!.length <= value.length) {
    value = line.pieces.pop()! + value;
  }
  line.pieces.push(value);
}

export async function* events(body: ReadableStream<Uint8Array>, signal: AbortSignal): AsyncGenerator<SseEvent> {
  const reader = body.getReader(), decoder = new TextDecoder('utf-8', { fatal: true });
  const abort = () => { void reader.cancel().catch(() => {}); };
  signal.addEventListener('abort', abort, { once: true });
  let line: Line = { pieces: [], bytes: 0 };
  let event = '', lines: string[] = [], bytes = 0;
  try {
    for (;;) {
      signal.throwIfAborted();
      const next = await reader.read();
      signal.throwIfAborted();
      const chunk = next.value ?? new Uint8Array();
      // A network chunk may contain several legal large events. Process bounded
      // slices before checking the unfinished line/frame buffer.
      for (let offset = 0; offset < chunk.length || (next.done && offset === 0); offset += 32 * 1024) {
      const part = chunk.subarray(offset, Math.min(chunk.length, offset + 32 * 1024));
      const text = decoder.decode(part, { stream: !(next.done && offset + part.length >= chunk.length) });
      let start = 0, end: number;
      while ((end = text.indexOf('\n', start)) >= 0) {
        append(line, text.slice(start, end), bytes); start = end + 1;
        let value = line.pieces.join('');
        bytes += line.bytes + 1; line = { pieces: [], bytes: 0 };
        if (value.endsWith('\r')) value = value.slice(0, -1);
        if (bytes > MAX_FRAME_BYTES) throw overflow();
        if (value === '') {
          if (!event && lines.length) throw malformed();
          if (event) {
            if (event !== 'next' && event !== 'complete') throw malformed();
            let data: unknown = null;
            if (lines.length) {
              try { data = JSON.parse(lines.join('\n')); } catch { throw malformed(); }
            } else if (event === 'next') throw malformed();
            yield { event, data };
            if (event === 'complete') return;
          }
          event = ''; lines = []; bytes = 0;
        } else if (!value.startsWith(':')) {
          const colon = value.indexOf(':');
          const name = colon < 0 ? value : value.slice(0, colon);
          let field = colon < 0 ? '' : value.slice(colon + 1);
          if (field.startsWith(' ')) field = field.slice(1);
          if (name === 'event') event = field;
          if (name === 'data') {
            if (lines.length >= 2048) throw overflow();
            lines.push(field);
          }
        }
      }
      append(line, text.slice(start), bytes);
      }
      if (next.done) {
        // A partial final frame is never promoted to a complete GraphQL result.
        throw new WireError('Live connection ended. Reconnecting…', 'connection_ended', false, true);
      }
    }
  } finally {
    signal.removeEventListener('abort', abort);
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
function overflow(): WireError { return new WireError('Live data exceeded its buffer. Reload the snapshot.', 'frame_overflow', true); }
function malformed(): WireError { return new WireError('Live data was incomplete. Reload the snapshot.', 'invalid_frame', true); }
