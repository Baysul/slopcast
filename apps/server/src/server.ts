import http from 'node:http';
import type {
  ClientOrigin,
  ClientRole,
  CreateRoomPayload,
  ErrorPayload,
  JoinedRoomPayload,
  JoinRoomPayload,
  PublishAckPayload,
  PublishRejectedPayload,
  PublishStreamPayload,
  RoleAssignmentPayload,
  RoomClosedPayload,
  RoomCreatedPayload,
  StopStreamPayload,
  WebRTCSignalPayload,
  WSMessage,
} from '@slopcast/shared-types';
import express, { type Request, type Response } from 'express';
import { WebSocket, WebSocketServer } from 'ws';
import { RoomManager } from './roomManager';

export interface ClientConnection {
  id: string;
  ws: WebSocket;
  roomCode: string | null;
  role: ClientRole | null;
  origin: ClientOrigin;
}

export function createServer(_port: number = 3001, baseUrl: string = 'http://localhost:3000') {
  const app = express();
  app.use(express.json());

  const roomManager = new RoomManager(baseUrl);
  const server = http.createServer(app);
  const wss = new WebSocketServer({ server });

  const clients: Map<string, ClientConnection> = new Map();

  // Helper to send typed JSON messages
  const sendMessage = <T>(ws: WebSocket, type: string, payload: T) => {
    if (ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type, payload }));
    }
  };

  // REST API Endpoints
  app.get('/health', (_req: Request, res: Response) => {
    res.json({ status: 'ok', activeRooms: roomManager.getAllRooms().length });
  });

  app.post('/api/rooms', (req: Request, res: Response) => {
    const { customCode } = req.body || {};
    const room = roomManager.createRoom(null, customCode);
    res.status(201).json({
      code: room.code,
      shareUrl: room.shareUrl,
      createdAt: room.createdAt,
    });
  });

  app.get('/api/rooms/:code', (req: Request, res: Response) => {
    const code = req.params.code as string;
    const room = roomManager.getRoom(code);
    if (!room) {
      return res.status(404).json({ error: 'Room not found' });
    }
    res.json(room);
  });

  // WebSocket Server Handling
  wss.on('connection', (ws: WebSocket, req: http.IncomingMessage) => {
    const clientId = Math.random().toString(36).substring(2, 10);

    // Detect origin from user-agent or query param / headers
    let clientOrigin: ClientOrigin = 'web';
    const userAgent = req.headers['user-agent'] || '';
    if (userAgent.includes('Electron') || req.headers['x-client-origin'] === 'desktop') {
      clientOrigin = 'desktop';
    }

    const conn: ClientConnection = {
      id: clientId,
      ws,
      roomCode: null,
      role: null,
      origin: clientOrigin,
    };

    clients.set(clientId, conn);

    ws.on('message', (data: string) => {
      try {
        const msg: WSMessage = JSON.parse(data.toString());
        handleMessage(conn, msg);
      } catch (_err) {
        sendMessage<ErrorPayload>(ws, 'ERROR', {
          message: 'Invalid JSON message format',
        });
      }
    });

    ws.on('close', () => {
      handleDisconnect(conn);
      clients.delete(clientId);
    });

    ws.on('error', (err) => {
      console.error(`Socket error for client ${clientId}:`, err);
    });
  });

  function handleMessage(conn: ClientConnection, msg: WSMessage) {
    const { type, payload } = msg;

    switch (type) {
      case 'CREATE_ROOM': {
        const p = (payload || {}) as CreateRoomPayload;
        if (p.clientOrigin) {
          conn.origin = p.clientOrigin;
        }

        const room = roomManager.createRoom(conn.id);
        const { role, reason } = roomManager.addParticipant(room.code, conn.id, conn.origin, 'presenter');

        conn.roomCode = room.code;
        conn.role = role;

        sendMessage<RoomCreatedPayload>(conn.ws, 'ROOM_CREATED', {
          code: room.code,
          shareUrl: room.shareUrl,
          role,
        });

        sendMessage<RoleAssignmentPayload>(conn.ws, 'ROLE_ASSIGNMENT', {
          role,
          reason,
        });
        break;
      }

      case 'JOIN_ROOM': {
        const p = payload as JoinRoomPayload;
        if (!p?.code) {
          sendMessage<ErrorPayload>(conn.ws, 'ERROR', {
            message: 'Room code is required to join a room',
          });
          return;
        }

        if (p.clientOrigin) {
          conn.origin = p.clientOrigin;
        }

        const roomCode = p.code;
        const requestedRole = p.requestedRole || (conn.origin === 'desktop' ? 'presenter' : 'spectator');

        try {
          const { participant, role, reason } = roomManager.addParticipant(
            roomCode,
            conn.id,
            conn.origin,
            requestedRole,
          );

          conn.roomCode = roomCode;
          conn.role = role;

          const room = roomManager.getRoom(roomCode);
          if (!room) {
            throw new Error(`Room ${roomCode} not found`);
          }
          const participantList = Object.values(room.participants);

          sendMessage<JoinedRoomPayload>(conn.ws, 'JOINED_ROOM', {
            code: roomCode,
            role,
            participants: participantList,
            isStreaming: !!room.isStreaming,
            presenterId: room.presenterId,
            assignedId: conn.id,
          });

          sendMessage<RoleAssignmentPayload>(conn.ws, 'ROLE_ASSIGNMENT', {
            role,
            reason,
          });

          // Notify other participants in room
          broadcastToRoomExcept(roomCode, conn.id, 'USER_JOINED', {
            participant,
          });

          // If the presenter is already live, tell the new spectator immediately so
          // the UI shifts to "waiting for offer", and the presenter will offer via
          // USER_JOINED on their side.
          if (room.isStreaming && room.presenterId && role === 'spectator') {
            sendMessage(conn.ws, 'PUBLISH_STREAM', {
              senderId: room.presenterId,
              streamId: 'active',
            });
          }
        } catch (err: unknown) {
          const message = err instanceof Error ? err.message : 'Failed to join room';
          sendMessage<ErrorPayload>(conn.ws, 'ERROR', {
            message,
          });
        }
        break;
      }

      case 'PUBLISH_STREAM': {
        const p = payload as PublishStreamPayload;

        // Role Enforcement Guardrail:
        // Reject stream publishing if origin is web or role is spectator
        if (conn.origin === 'web' || conn.role !== 'presenter') {
          sendMessage<PublishRejectedPayload>(conn.ws, 'PUBLISH_REJECTED', {
            reason:
              conn.origin === 'web'
                ? 'Web clients are restricted to spectator-only mode and cannot publish streams.'
                : 'Only presenter connections are authorized to publish video/audio streams.',
          });
          return;
        }

        if (conn.roomCode) {
          roomManager.setStreaming(conn.roomCode, true);
          const spectatorIds = roomManager.getSpectatorIds(conn.roomCode);

          // Authoritative spectator list so the presenter can create offers even
          // if it missed USER_JOINED notifications.
          sendMessage<PublishAckPayload>(conn.ws, 'PUBLISH_ACK', {
            streamId: p?.streamId,
            spectatorIds,
          });

          // Broadcast stream publish notice to room spectators
          broadcastToRoomExcept(conn.roomCode, conn.id, 'PUBLISH_STREAM', {
            senderId: conn.id,
            streamId: p?.streamId,
            metadata: p?.metadata,
          });

          console.log(
            `[PUBLISH_STREAM] room=${conn.roomCode} presenter=${conn.id} spectators=[${spectatorIds.join(',')}]`,
          );
        }
        break;
      }

      case 'WEBRTC_SIGNAL': {
        const p = payload as WebRTCSignalPayload;
        if (!conn.roomCode) {
          sendMessage<ErrorPayload>(conn.ws, 'ERROR', {
            message: 'Must be in a room to send WebRTC signals',
          });
          return;
        }

        // Role Enforcement Guardrail on WebRTC signals:
        // Spectators/web clients cannot send offer signals attempting to start an outbound stream
        if (
          (conn.origin === 'web' || conn.role === 'spectator') &&
          p.signal &&
          (p.signal as Record<string, unknown>).type === 'offer'
        ) {
          sendMessage<PublishRejectedPayload>(conn.ws, 'PUBLISH_REJECTED', {
            reason: 'Spectator clients cannot initiate outbound WebRTC screen offers.',
          });
          return;
        }

        const signalPayload = {
          ...p,
          senderId: conn.id,
        };

        if (p.targetId) {
          const targetConn = clients.get(p.targetId);
          if (targetConn && targetConn.roomCode === conn.roomCode) {
            sendMessage(targetConn.ws, 'WEBRTC_SIGNAL', signalPayload);
          } else {
            console.warn(
              `[WEBRTC_SIGNAL] target ${p.targetId} not found in room ${conn.roomCode} (from ${conn.id}, type=${(p.signal as Record<string, unknown> | null)?.type})`,
            );
          }
        } else {
          // If no target specified:
          // If sender is presenter, broadcast to all spectators in room
          // If sender is spectator, send to presenter
          const room = roomManager.getRoom(conn.roomCode);
          if (room) {
            if (conn.role === 'presenter') {
              broadcastToRoomExcept(conn.roomCode, conn.id, 'WEBRTC_SIGNAL', signalPayload);
            } else if (room.presenterId) {
              const presenterConn = clients.get(room.presenterId);
              if (presenterConn) {
                sendMessage(presenterConn.ws, 'WEBRTC_SIGNAL', signalPayload);
              }
            }
          }
        }
        break;
      }

      case 'STOP_STREAM': {
        if (!conn.roomCode || conn.role !== 'presenter') {
          sendMessage<ErrorPayload>(conn.ws, 'ERROR', {
            message: 'Only presenters can stop a stream.',
          });
          return;
        }

        roomManager.setStreaming(conn.roomCode, false);

        broadcastToRoomExcept<StopStreamPayload>(conn.roomCode, conn.id, 'STOP_STREAM', {
          senderId: conn.id,
        });

        console.log(`[STOP_STREAM] room=${conn.roomCode} presenter=${conn.id}`);
        break;
      }

      default: {
        sendMessage<ErrorPayload>(conn.ws, 'ERROR', {
          message: `Unknown message type: ${type}`,
        });
      }
    }
  }

  function handleDisconnect(conn: ClientConnection) {
    if (!conn.roomCode) return;

    const { isPresenter } = roomManager.removeParticipant(conn.roomCode, conn.id);

    if (isPresenter) {
      // Presenter left: close room and notify spectators
      broadcastToRoomExcept<RoomClosedPayload>(conn.roomCode, conn.id, 'ROOM_CLOSED', {
        reason: 'The presenter has left the room. Session ended.',
      });
      roomManager.closeRoom(conn.roomCode);
    } else {
      // Spectator left: notify remaining participants
      broadcastToRoomExcept(conn.roomCode, conn.id, 'USER_LEFT', {
        userId: conn.id,
      });
    }
  }

  function broadcastToRoomExcept<T>(roomCode: string, excludeClientId: string, type: string, payload: T) {
    for (const client of clients.values()) {
      if (client.roomCode === roomCode && client.id !== excludeClientId) {
        sendMessage(client.ws, type, payload);
      }
    }
  }

  return { app, server, wss, roomManager, clients };
}
