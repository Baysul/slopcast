import type {
  ErrorPayload,
  JoinedRoomPayload,
  JoinRoomPayload,
  Participant,
  PublishStreamPayload,
  RoleAssignmentPayload,
  RoomClosedPayload,
  StopStreamPayload,
  WebRTCSignalPayload,
  WSMessage,
} from '@slopcast/shared-types';

export type SignalingEventMap = {
  connected: () => void;
  disconnected: (reason?: string) => void;
  joined_room: (payload: JoinedRoomPayload) => void;
  role_assignment: (payload: RoleAssignmentPayload) => void;
  user_joined: (participant: Participant) => void;
  user_left: (userId: string) => void;
  publish_stream: (payload: PublishStreamPayload & { senderId: string }) => void;
  stop_stream: (payload: StopStreamPayload) => void;
  webrtc_signal: (payload: WebRTCSignalPayload & { senderId: string }) => void;
  room_closed: (payload: RoomClosedPayload) => void;
  error: (payload: ErrorPayload) => void;
};

type EventCallback<T extends keyof SignalingEventMap> = SignalingEventMap[T];

declare global {
  interface Window {
    __SLOPCAST_CONFIG__?: {
      apiEndpoint?: string;
    };
  }
}

export class SignalingClient {
  private ws: WebSocket | null = null;
  private url: string;
  private listeners: Partial<{ [K in keyof SignalingEventMap]: EventCallback<K>[] }> = {};
  private intentionalClose = false;
  private connectSettled = false;
  private settleConnect: ((ok: boolean, err?: unknown) => void) | null = null;

  constructor(url?: string) {
    if (url) {
      this.url = url;
    } else if (typeof window !== 'undefined' && window.__SLOPCAST_CONFIG__?.apiEndpoint) {
      this.url = window.__SLOPCAST_CONFIG__.apiEndpoint.replace(/^http/, 'ws');
    } else if (typeof window !== 'undefined') {
      const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
      const host = window.location.hostname || 'localhost';
      const port = window.location.port || '3001';
      this.url = `${protocol}//${host}:${port}`;
    } else {
      this.url = 'ws://localhost:3001';
    }
  }

  public getUrl(): string {
    return this.url;
  }

  public connect(customWsClass?: typeof WebSocket): Promise<void> {
    return new Promise((resolve, reject) => {
      try {
        this.intentionalClose = false;
        this.connectSettled = false;
        this.settleConnect = (ok, err) => {
          if (this.connectSettled) return;
          this.connectSettled = true;
          this.settleConnect = null;
          if (ok) resolve();
          else reject(err ?? new Error('WebSocket connection failed'));
        };

        const WSImpl = customWsClass || (typeof window !== 'undefined' ? window.WebSocket : WebSocket);
        const ws = new WSImpl(this.url);
        this.ws = ws;

        ws.onopen = () => {
          if (this.intentionalClose || this.ws !== ws) {
            // Abandoned during CONNECTING (e.g. React Strict Mode remount) — close quietly now.
            try {
              ws.close();
            } catch {
              /* ignore */
            }
            return;
          }
          this.emit('connected');
          this.settleConnect?.(true);
        };

        ws.onmessage = (event: MessageEvent) => {
          if (this.ws !== ws || this.intentionalClose) return;
          try {
            const message: WSMessage = JSON.parse(event.data);
            this.handleMessage(message);
          } catch (err) {
            console.error('[SignalingClient] Failed to parse message:', err);
          }
        };

        ws.onclose = (event: CloseEvent) => {
          if (this.ws === ws) {
            this.ws = null;
          }
          if (!this.intentionalClose) {
            this.emit('disconnected', event.reason || 'Server connection closed');
            this.settleConnect?.(false, new Error('WebSocket closed before opening'));
          } else {
            // Soft-cancel: resolve so callers aren't left hanging; they must not treat this asconnected
            // (connected event / emit only fires on meaningful open above).
            this.settleConnect?.(true);
          }
        };

        ws.onerror = () => {
          if (this.intentionalClose || this.ws !== ws) return;
          this.emit('error', { message: 'Failed to connect to signaling server' });
          this.settleConnect?.(false, new Error('WebSocket error'));
        };
      } catch (err) {
        reject(err);
      }
    });
  }

  public joinRoom(roomCode: string) {
    const payload: JoinRoomPayload = {
      code: roomCode,
      clientOrigin: 'web',
      requestedRole: 'spectator',
    };
    this.send('JOIN_ROOM', payload);
  }

  public sendSignal(targetId: string | undefined, signal: unknown) {
    const payload: WebRTCSignalPayload = {
      targetId,
      signal,
    };
    this.send('WEBRTC_SIGNAL', payload);
  }

  private send<T>(type: string, payload: T) {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify({ type, payload }));
    } else {
      console.warn('[SignalingClient] Cannot send message, WebSocket not connected.');
    }
  }

  private handleMessage(msg: WSMessage) {
    switch (msg.type) {
      case 'JOINED_ROOM':
        this.emit('joined_room', msg.payload as JoinedRoomPayload);
        break;
      case 'ROLE_ASSIGNMENT':
        this.emit('role_assignment', msg.payload as RoleAssignmentPayload);
        break;
      case 'USER_JOINED':
        this.emit('user_joined', (msg.payload as { participant: Participant }).participant);
        break;
      case 'USER_LEFT':
        this.emit('user_left', (msg.payload as { userId: string }).userId);
        break;
      case 'PUBLISH_STREAM':
        this.emit('publish_stream', msg.payload as PublishStreamPayload & { senderId: string });
        break;
      case 'STOP_STREAM':
        this.emit('stop_stream', msg.payload as StopStreamPayload);
        break;
      case 'WEBRTC_SIGNAL':
        this.emit('webrtc_signal', msg.payload as WebRTCSignalPayload & { senderId: string });
        break;
      case 'ROOM_CLOSED':
        this.emit('room_closed', msg.payload as RoomClosedPayload);
        break;
      case 'ERROR':
        this.emit('error', msg.payload as ErrorPayload);
        break;
    }
  }

  public on<K extends keyof SignalingEventMap>(event: K, callback: EventCallback<K>) {
    if (!this.listeners[event]) {
      this.listeners[event] = [];
    }
    (this.listeners[event] as EventCallback<K>[]).push(callback);
  }

  public off<K extends keyof SignalingEventMap>(event: K, callback: EventCallback<K>) {
    const arr = this.listeners[event] as EventCallback<K>[] | undefined;
    if (!arr) return;
    for (let i = arr.length - 1; i >= 0; i--) {
      if (arr[i] === callback) arr.splice(i, 1);
    }
  }

  private emit<K extends keyof SignalingEventMap>(event: K, ...args: Parameters<SignalingEventMap[K]>) {
    const eventListeners = this.listeners[event];
    if (eventListeners) {
      for (const cb of eventListeners) {
        (cb as (...a: Parameters<SignalingEventMap[K]>) => void)(...args);
      }
    }
  }

  public disconnect() {
    this.intentionalClose = true;
    this.listeners = {};

    const ws = this.ws;
    this.ws = null;
    if (!ws) {
      this.settleConnect?.(true);
      return;
    }

    // Never close() while CONNECTING — browsers log
    // "WebSocket is closed before the connection is established".
    // Detach ownership and close only once OPEN (or leave it to fail/error).
    if (ws.readyState === WebSocket.CONNECTING) {
      ws.onmessage = null;
      ws.onerror = null;
      ws.onclose = () => {
        this.settleConnect?.(true);
      };
      ws.onopen = () => {
        try {
          ws.close();
        } catch {
          /* ignore */
        }
      };
      // Pending connect is cancelled; RoomPage treats this via generation / no `connected` emit.
      this.settleConnect?.(true);
      return;
    }

    ws.onopen = null;
    ws.onmessage = null;
    ws.onerror = null;
    ws.onclose = null;
    if (ws.readyState === WebSocket.OPEN) {
      try {
        ws.close();
      } catch {
        /* ignore */
      }
    }
    this.settleConnect?.(true);
  }
}

export default SignalingClient;
