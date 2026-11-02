import { isJsonObject, readString } from '@slopcast/shared-types';

export interface HeartbeatRoom {
  readonly apiEndpoint: string;
  readonly closeKey: string;
  readonly code: string;
}

export type HeartbeatResult =
  | { readonly kind: 'ok'; readonly token: string }
  // The server no longer knows the room (closed elsewhere, or reaped after the presenter vanished).
  | { readonly kind: 'closed' }
  // Network trouble, rate limiting, a restarting server: keep streaming and try again later.
  | { readonly kind: 'retry'; readonly reason: string }
  // A server without the heartbeat route; nothing to refresh, but nothing is wrong either.
  | { readonly kind: 'unsupported' };

export const heartbeatResult = (status: number, body: unknown): HeartbeatResult => {
  if (status === 200) {
    const token = isJsonObject(body) ? readString(body, 'token') : undefined;
    if (token === undefined || token === '') {
      return { kind: 'retry', reason: 'heartbeat response is missing a token' };
    }
    return { kind: 'ok', token };
  }

  // Only the route's own JSON 404 means "room gone"; a bare 404 is an older server.
  const hasErrorBody = isJsonObject(body) && readString(body, 'error') !== undefined;
  if (status === 404) {
    return hasErrorBody ? { kind: 'closed' } : { kind: 'unsupported' };
  }
  if (status === 403 && hasErrorBody) {
    return { kind: 'closed' };
  }
  return { kind: 'retry', reason: `server returned ${status}` };
};

const readJsonBody = async (response: Response): Promise<unknown> => {
  try {
    const body: unknown = await response.json();
    return body;
  } catch {
    return null;
  }
};

export const sendPresenterHeartbeat = async (room: HeartbeatRoom, timeoutMs: number): Promise<HeartbeatResult> => {
  try {
    const response = await fetch(`${room.apiEndpoint}/api/rooms/${room.code}/presenter-token`, {
      headers: { 'X-Room-Close-Key': room.closeKey },
      method: 'POST',
      signal: AbortSignal.timeout(timeoutMs),
    });
    return heartbeatResult(response.status, await readJsonBody(response));
  } catch (error) {
    return { kind: 'retry', reason: error instanceof Error ? error.message : String(error) };
  }
};
