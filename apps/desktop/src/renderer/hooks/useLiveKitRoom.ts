import type { VideoCodec } from '@slopcast/shared-types';
import { Room, RoomEvent } from 'livekit-client';
import { useCallback, useRef, useState } from 'react';
import { notify, primeAudioContext } from '../lib/toast';
import '../types/electron-api.d.ts';

export interface UseLiveKitRoomOptions {
  apiEndpoint: string;
  livekitUrl: string;
  videoCodec: VideoCodec;
  onDisconnect?: () => void;
}

export interface UseLiveKitRoomReturn {
  roomCode: string;
  shareUrl: string;
  spectatorCount: number;
  isCreatingRoom: boolean;
  liveKitRoomRef: React.RefObject<Room | null>;
  createRoom: () => Promise<void>;
  disconnectRoom: () => void;
}

export function useLiveKitRoom({
  apiEndpoint,
  livekitUrl,
  videoCodec,
  onDisconnect,
}: UseLiveKitRoomOptions): UseLiveKitRoomReturn {
  const [roomCode, setRoomCode] = useState<string>('');
  const [shareUrl, setShareUrl] = useState<string>('');
  const [spectatorCount, setSpectatorCount] = useState<number>(0);
  const [isCreatingRoom, setIsCreatingRoom] = useState<boolean>(false);

  const liveKitRoomRef = useRef<Room | null>(null);

  const disconnectRoom = useCallback(() => {
    const room = liveKitRoomRef.current;
    if (room) {
      room.removeAllListeners();
      room.disconnect();
      liveKitRoomRef.current = null;
    }
    setRoomCode('');
    setShareUrl('');
    setSpectatorCount(0);
  }, []);

  const createRoom = useCallback(async () => {
    if (isCreatingRoom) return;
    setIsCreatingRoom(true);
    primeAudioContext();

    disconnectRoom();

    try {
      const res = await fetch(`${apiEndpoint}/api/rooms`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'X-Client-Origin': 'desktop' },
      });

      if (!res.ok) {
        const err = (await res.json().catch(() => ({ error: 'Unknown server error' }))) as { error?: string };
        throw new Error(err.error ?? `Server returned ${res.status}`);
      }

      const room = (await res.json()) as {
        code: string;
        shareUrl: string;
        token: string;
        livekitUrl: string;
      };
      const code = room.code;
      const url = room.shareUrl;
      const token = room.token;
      const apiLivekitUrl = room.livekitUrl;
      const resolvedLivekitUrl = livekitUrl || apiLivekitUrl;

      setRoomCode(code);
      setShareUrl(url);
      setSpectatorCount(0);

      const lkRoom = new Room({
        publishDefaults: {
          videoCodec,
        },
      });
      liveKitRoomRef.current = lkRoom;

      lkRoom.on(RoomEvent.ParticipantConnected, (participant) => {
        if (!participant.isLocal) {
          setSpectatorCount(lkRoom.remoteParticipants.size);
        }
      });

      lkRoom.on(RoomEvent.ParticipantDisconnected, (participant) => {
        if (!participant.isLocal) {
          setSpectatorCount(lkRoom.remoteParticipants.size);
        }
      });

      lkRoom.on(RoomEvent.Disconnected, () => {
        if (liveKitRoomRef.current === lkRoom) {
          notify('error', 'Room disconnected', 'The connection to the room was lost. Create a new room to continue.');
          onDisconnect?.();
          setRoomCode('');
          setShareUrl('');
          setSpectatorCount(0);
          liveKitRoomRef.current = null;
        }
      });

      await lkRoom.connect(resolvedLivekitUrl, token);
    } catch (err) {
      console.error('Failed to create room:', err);
      const message = err instanceof Error ? err.message : 'Failed to create room';
      notify('error', 'Room creation failed', message);
    } finally {
      setIsCreatingRoom(false);
    }
  }, [apiEndpoint, livekitUrl, videoCodec, disconnectRoom, isCreatingRoom, onDisconnect]);

  return {
    roomCode,
    shareUrl,
    spectatorCount,
    isCreatingRoom,
    liveKitRoomRef,
    createRoom,
    disconnectRoom,
  };
}
