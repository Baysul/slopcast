export interface AppConfig {
  serverPort: number;
  webPort: number;
  apiEndpoint: string;
  websiteUrl: string;
}

export type ClientRole = 'presenter' | 'spectator';
export type ClientOrigin = 'desktop' | 'web';

export interface Participant {
  id: string;
  role: ClientRole;
  origin: ClientOrigin;
  joinedAt: number;
}

export interface RoomState {
  code: string;
  shareUrl: string;
  createdAt: number;
  presenterId: string | null;
  participants: Record<string, Participant>;
  /** True after the presenter has published a live stream. */
  isStreaming?: boolean;
}

export type MessageType =
  | 'CREATE_ROOM'
  | 'ROOM_CREATED'
  | 'JOIN_ROOM'
  | 'JOINED_ROOM'
  | 'ROLE_ASSIGNMENT'
  | 'WEBRTC_SIGNAL'
  | 'PUBLISH_STREAM'
  | 'PUBLISH_ACK'
  | 'PUBLISH_REJECTED'
  | 'STOP_STREAM'
  | 'USER_JOINED'
  | 'USER_LEFT'
  | 'ROOM_CLOSED'
  | 'ERROR';

export interface CreateRoomPayload {
  clientOrigin: ClientOrigin;
}

export interface RoomCreatedPayload {
  code: string;
  shareUrl: string;
  role: ClientRole;
}

export interface JoinRoomPayload {
  code: string;
  clientOrigin: ClientOrigin;
  requestedRole?: ClientRole;
}

export interface JoinedRoomPayload {
  code: string;
  role: ClientRole;
  participants: Participant[];
  isStreaming?: boolean;
  presenterId?: string | null;
  assignedId?: string;
}

export interface RoleAssignmentPayload {
  role: ClientRole;
  reason?: string;
}

export interface WebRTCSignalPayload {
  targetId?: string;
  senderId?: string;
  signal: unknown;
}

export interface PublishStreamPayload {
  streamId?: string;
  metadata?: Record<string, unknown>;
}

/** Server → presenter after a successful PUBLISH_STREAM. */
export interface PublishAckPayload {
  streamId?: string;
  spectatorIds: string[];
}

export interface PublishRejectedPayload {
  reason: string;
}

export interface RoomClosedPayload {
  reason: string;
}

/** Presenter → server when stopping a live stream. Server → spectators after the fact. */
export interface StopStreamPayload {
  senderId?: string;
}

export interface ErrorPayload {
  message: string;
  code?: string;
}

export interface WSMessage<T = unknown> {
  type: MessageType;
  payload: T;
}
