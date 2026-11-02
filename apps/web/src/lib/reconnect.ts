import { DisconnectReason } from 'livekit-client';

export { DisconnectReason };

// LiveKit resumes short interruptions itself and only reports `Disconnected` after giving up
// (roughly 40 s of attempts). From there the page rejoins with a fresh token on this schedule.
export const RECONNECT_BASE_MS = 1000;
export const RECONNECT_MAX_MS = 15_000;
// After this many failed rejoins the page shows the error and a "Retry now" button.
export const RECONNECT_ATTEMPTS_BEFORE_NOTICE = 3;

export type DisconnectOutcome = 'ignore' | 'ended' | 'rejoin';

export const classifyDisconnect = (reason: DisconnectReason | undefined): DisconnectOutcome => {
  switch (reason) {
    case DisconnectReason.CLIENT_INITIATED:
      return 'ignore';
    case DisconnectReason.ROOM_DELETED:
    case DisconnectReason.ROOM_CLOSED:
    case DisconnectReason.PARTICIPANT_REMOVED:
      return 'ended';
    default:
      return 'rejoin';
  }
};

// Full jitter keeps a room full of spectators from stampeding the API after an outage.
export const reconnectDelayMs = (attempt: number, random: () => number = Math.random): number => {
  const ceiling = Math.min(RECONNECT_MAX_MS, RECONNECT_BASE_MS * 2 ** Math.max(0, attempt));
  return Math.round(ceiling / 2 + (ceiling / 2) * random());
};

export class TokenRequestError extends Error {
  readonly status: number;

  constructor(message: string, status: number) {
    super(message);
    this.name = 'TokenRequestError';
    this.status = status;
  }
}

// The API answers 404 only once a room is closed (it adopts live rooms across restarts), and
// 400 for a malformed code. Everything else, including network errors, is worth retrying.
export const isRoomGone = (error: unknown): boolean =>
  error instanceof TokenRequestError && (error.status === 404 || error.status === 400);
