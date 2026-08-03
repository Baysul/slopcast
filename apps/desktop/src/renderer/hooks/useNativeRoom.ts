import { useCallback, useEffect, useRef, useState } from 'react';
import { notify, primeAudioContext } from '../lib/toast';
import '../types/electron-api.d.ts';

const POLL_MS = 1000;

export interface UseNativeRoomOptions {
  apiEndpoint: string;
  livekitUrl: string;
  onDisconnect?: () => void;
}

export interface UseNativeRoomReturn {
  roomCode: string;
  shareUrl: string;
  spectatorCount: number;
  isCreatingRoom: boolean;
  createRoom: () => Promise<void>;
  disconnectRoom: () => void;
}

// The renderer's only room path: room creation mints a presenter token from
// the server, then the connection itself lives in native-livekit (main
// process). Spectator count and connection state are polled over IPC since
// native-livekit does not push events to the renderer.
export function useNativeRoom({ apiEndpoint, livekitUrl, onDisconnect }: UseNativeRoomOptions): UseNativeRoomReturn {
  const [roomCode, setRoomCode] = useState<string>('');
  const [shareUrl, setShareUrl] = useState<string>('');
  const [spectatorCount, setSpectatorCount] = useState<number>(0);
  const [isCreatingRoom, setIsCreatingRoom] = useState<boolean>(false);

  const roomActiveRef = useRef(false);
  // connectNativeRoom returns before the worker finishes joining (ROOM_CONNECTED
  // flips only after the audio track publishes), so a transient false on the
  // first polls must not be treated as a disconnect.
  const sawConnectedRef = useRef(false);

  const disconnectRoom = useCallback(() => {
    void window.electronAPI?.disconnectNativeRoom();
    roomActiveRef.current = false;
    sawConnectedRef.current = false;
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
      const resolvedLivekitUrl = room.livekitUrl || livekitUrl;

      const connected = await window.electronAPI?.connectNativeRoom(resolvedLivekitUrl, room.token);
      if (!connected) {
        throw new Error('Native LiveKit connection failed');
      }

      roomActiveRef.current = true;
      setRoomCode(room.code);
      setShareUrl(room.shareUrl);
      setSpectatorCount(0);
    } catch (err) {
      console.error('Failed to create room:', err);
      const message = err instanceof Error ? err.message : 'Failed to create room';
      notify('error', 'Room creation failed', message);
    } finally {
      setIsCreatingRoom(false);
    }
  }, [apiEndpoint, livekitUrl, disconnectRoom, isCreatingRoom]);

  // Poll spectator count and detect an unexpected room drop (native-livekit
  // has no event push to the renderer).
  useEffect(() => {
    if (!roomCode) return;

    const poll = async (): Promise<void> => {
      const count = await window.electronAPI?.getSpectatorCount();
      if (typeof count === 'number') {
        setSpectatorCount(count);
      }
      const connected = await window.electronAPI?.isNativeRoomConnected();
      if (connected === true) {
        sawConnectedRef.current = true;
      }
      if (connected === false && sawConnectedRef.current && roomActiveRef.current) {
        notify('error', 'Room disconnected', 'The connection to the room was lost. Create a new room to continue.');
        onDisconnect?.();
        roomActiveRef.current = false;
        setRoomCode('');
        setShareUrl('');
        setSpectatorCount(0);
      }
    };

    void poll();
    const interval = setInterval(() => void poll(), POLL_MS);
    return () => clearInterval(interval);
  }, [roomCode, onDisconnect]);

  return {
    roomCode,
    shareUrl,
    spectatorCount,
    isCreatingRoom,
    createRoom,
    disconnectRoom,
  };
}
