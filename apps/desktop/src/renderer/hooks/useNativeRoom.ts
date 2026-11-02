import { isJsonObject, readFiniteNumber, readString } from '@slopcast/shared-types';
import type { JsonObject } from '@slopcast/shared-types';
import { useCallback, useEffect, useRef, useState } from 'react';

import { desktopApi } from '../api/desktop';
import { notify, primeAudioContext } from '../lib/toast';
import { sendPresenterHeartbeat } from '../utils/presenter-heartbeat';

const POLL_MS = 1000;
// Well inside the server's presenter grace period, so a reachable presenter is never reaped.
const HEARTBEAT_MS = 20_000;
const HEARTBEAT_TIMEOUT_MS = 10_000;
// Brief blips that LiveKit resumes on its own are not worth a toast.
const RECONNECT_NOTICE_DELAY_MS = 3000;

export type RoomConnectionState = 'connecting' | 'connected' | 'reconnecting';

export interface PreparedRoom {
  apiEndpoint: string;
  closeKey: string;
  code: string;
  identity: string;
  livekitUrl: string;
  shareUrl: string;
  token: string;
}

interface RoomOperationResult {
  ok: boolean;
  error?: string;
}

const readRequiredString = (body: JsonObject, key: string): string => {
  const value = readString(body, key);
  if (value === undefined) {
    throw new Error(`Room response is missing ${key}`);
  }
  return value;
};

const readErrorMessage = async (response: Response, fallback: string): Promise<string> => {
  try {
    const body: unknown = await response.json();
    const message = isJsonObject(body) ? readString(body, 'error') : undefined;
    if (message !== undefined && message.length > 0) {
      return message;
    }
  } catch {
    return fallback;
  }

  return fallback;
};

const fetchSpectatorCount = async (room: PreparedRoom): Promise<number | null> => {
  try {
    const response = await fetch(`${room.apiEndpoint}/api/rooms/${room.code}/spectators`);
    if (!response.ok) {
      return null;
    }

    const body: unknown = await response.json();
    const count = isJsonObject(body) ? readFiniteNumber(body, 'count') : undefined;
    if (count === undefined) {
      return null;
    }

    return count;
  } catch (error) {
    console.warn('Transient spectator count failure:', error);
    return null;
  }
};

const requestRoom = async (apiEndpoint: string): Promise<PreparedRoom> => {
  const response = await fetch(`${apiEndpoint}/api/rooms`, {
    headers: { 'Content-Type': 'application/json', 'X-Client-Origin': 'desktop' },
    method: 'POST',
  });
  if (!response.ok) {
    throw new Error(await readErrorMessage(response, `Server returned ${response.status}`));
  }

  const body: unknown = await response.json();
  if (!isJsonObject(body)) {
    throw new Error('Room response was not a JSON object');
  }

  return {
    apiEndpoint,
    closeKey: readRequiredString(body, 'closeKey'),
    code: readRequiredString(body, 'code'),
    identity: readRequiredString(body, 'identity'),
    livekitUrl: readRequiredString(body, 'livekitUrl'),
    shareUrl: readRequiredString(body, 'shareUrl'),
    token: readRequiredString(body, 'token'),
  };
};

const deleteRoom = async (room: PreparedRoom, keepalive = false): Promise<RoomOperationResult> => {
  try {
    const response = await fetch(`${room.apiEndpoint}/api/rooms/${room.code}`, {
      headers: { 'X-Room-Close-Key': room.closeKey },
      keepalive,
      method: 'DELETE',
    });
    if (response.ok || response.status === 404) {
      return { ok: true };
    }

    return { error: await readErrorMessage(response, `Server returned ${response.status}`), ok: false };
  } catch {
    return { error: 'The API could not be reached. Check the endpoint and try again.', ok: false };
  }
};

export interface UseNativeRoomOptions {
  apiEndpoint: string;
  livekitUrl: string;
  onDisconnect?: () => void;
}

export interface UseNativeRoomReturn {
  roomCode: string;
  connectionState: RoomConnectionState | null;
  roomEndpoint: string | null;
  shareUrl: string;
  spectatorCount: number;
  isCreatingRoom: boolean;
  isClosingRoom: boolean;
  createRoom: () => Promise<boolean>;
  prepareRoom: (apiEndpoint: string) => Promise<PreparedRoom | null>;
  connectPreparedRoom: (room: PreparedRoom) => Promise<RoomOperationResult>;
  discardPreparedRoom: (room: PreparedRoom) => Promise<void>;
  closeRoom: () => Promise<RoomOperationResult>;
  closeRoomBestEffort: () => void;
}

export const useNativeRoom = ({ apiEndpoint, livekitUrl, onDisconnect }: UseNativeRoomOptions): UseNativeRoomReturn => {
  const [activeRoom, setActiveRoom] = useState<PreparedRoom | null>(null);
  const [spectatorCount, setSpectatorCount] = useState(0);
  const [isCreatingRoom, setIsCreatingRoom] = useState(false);
  const [isClosingRoom, setIsClosingRoom] = useState(false);
  const [connectionState, setConnectionState] = useState<RoomConnectionState | null>(null);
  const activeRoomRef = useRef<PreparedRoom | null>(null);
  const sawConnectedRef = useRef(false);
  const isTransitioningRef = useRef(false);
  const lostSinceRef = useRef<number | null>(null);
  const announcedLossRef = useRef(false);

  const clearRoom = useCallback((): void => {
    activeRoomRef.current = null;
    sawConnectedRef.current = false;
    lostSinceRef.current = null;
    announcedLossRef.current = false;
    setActiveRoom(null);
    setConnectionState(null);
    setSpectatorCount(0);
  }, []);

  const prepareRoom = useCallback(async (endpoint: string): Promise<PreparedRoom | null> => {
    setIsCreatingRoom(true);
    primeAudioContext();
    try {
      return await requestRoom(endpoint);
    } catch (error) {
      console.error('Failed to prepare room:', error);
      const message = error instanceof Error ? error.message : 'Failed to prepare room';
      notify('error', 'Room creation failed', message);
      return null;
    } finally {
      setIsCreatingRoom(false);
    }
  }, []);

  const connectPreparedRoom = useCallback(
    async (room: PreparedRoom): Promise<RoomOperationResult> => {
      const resolvedLivekitUrl = room.livekitUrl || livekitUrl;
      const error = await desktopApi.connectNativeRoom(resolvedLivekitUrl, room.token, room.code, room.identity);
      if (error !== null) {
        return { error, ok: false };
      }

      activeRoomRef.current = room;
      sawConnectedRef.current = false;
      lostSinceRef.current = null;
      announcedLossRef.current = false;
      setActiveRoom(room);
      setConnectionState('connecting');
      setSpectatorCount(0);
      return { ok: true };
    },
    [livekitUrl],
  );

  const discardPreparedRoom = useCallback(async (room: PreparedRoom): Promise<void> => {
    const result = await deleteRoom(room);
    if (!result.ok) {
      console.warn(`Failed to discard prepared room ${room.code}: ${result.error}`);
    }
  }, []);

  const createRoom = useCallback(async (): Promise<boolean> => {
    if (isCreatingRoom || activeRoomRef.current) {
      return false;
    }
    const room = await prepareRoom(apiEndpoint);
    if (!room) {
      return false;
    }

    const result = await connectPreparedRoom(room);
    if (result.ok) {
      return true;
    }

    await discardPreparedRoom(room);
    notify('error', 'Room connection failed', result.error ?? 'The LiveKit room could not be reached.');
    return false;
  }, [apiEndpoint, connectPreparedRoom, discardPreparedRoom, isCreatingRoom, prepareRoom]);

  const closeRoom = useCallback(async (): Promise<RoomOperationResult> => {
    const room = activeRoomRef.current;
    if (!room) {
      return { ok: true };
    }
    setIsClosingRoom(true);
    isTransitioningRef.current = true;

    const result = await deleteRoom(room);
    if (!result.ok) {
      isTransitioningRef.current = false;
      setIsClosingRoom(false);
      return result;
    }

    await desktopApi.disconnectNativeRoom();
    clearRoom();
    isTransitioningRef.current = false;
    setIsClosingRoom(false);
    return { ok: true };
  }, [clearRoom]);

  const closeRoomBestEffort = useCallback((): void => {
    const room = activeRoomRef.current;
    if (room) {
      void deleteRoom(room, true);
    }
    void desktopApi.disconnectNativeRoom();
    clearRoom();
  }, [clearRoom]);

  useEffect(
    () => () => {
      const room = activeRoomRef.current;
      if (room) {
        void deleteRoom(room, true);
      }
      void desktopApi.disconnectNativeRoom();
    },
    [],
  );

  useEffect(() => {
    if (!activeRoom) {
      return undefined;
    }

    const poll = async (): Promise<void> => {
      const nextSpectatorCount = await fetchSpectatorCount(activeRoom);
      if (nextSpectatorCount !== null) {
        setSpectatorCount(nextSpectatorCount);
      }

      const connected = await desktopApi.isNativeRoomConnected();
      if (activeRoomRef.current?.code !== activeRoom.code || isTransitioningRef.current) {
        return undefined;
      }
      if (connected) {
        sawConnectedRef.current = true;
        lostSinceRef.current = null;
        if (announcedLossRef.current) {
          announcedLossRef.current = false;
          notify('success', 'Reconnected', 'Spectators are receiving your stream again.');
        }
        setConnectionState('connected');
        return undefined;
      }
      if (!sawConnectedRef.current) {
        return undefined;
      }

      const hasSession = await desktopApi.hasNativeRoomSession();
      if (activeRoomRef.current?.code !== activeRoom.code || isTransitioningRef.current) {
        return undefined;
      }
      if (hasSession) {
        // The native session is rejoining the room on its own; surface it, but keep the room.
        setConnectionState('reconnecting');
        const now = Date.now();
        lostSinceRef.current ??= now;
        if (!announcedLossRef.current && now - lostSinceRef.current >= RECONNECT_NOTICE_DELAY_MS) {
          announcedLossRef.current = true;
          notify('info', 'Connection lost', 'Reconnecting to the room. Your stream resumes automatically.');
        }
        return undefined;
      }

      notify('error', 'Room disconnected', 'The connection to the room was lost. Create a new room to continue.');
      clearRoom();
      onDisconnect?.();
    };

    void poll();
    const interval = setInterval(() => void poll(), POLL_MS);
    return () => {
      clearInterval(interval);
    };
  }, [activeRoom, clearRoom, onDisconnect]);

  // Presenter heartbeat: keeps the room alive on the server while the stream reconnects, and
  // Keeps the native session's rejoin token fresh for streams that outlive the original token.
  useEffect(() => {
    if (!activeRoom) {
      return undefined;
    }

    let isActive = true;
    const beat = async (): Promise<void> => {
      const result = await sendPresenterHeartbeat(activeRoom, HEARTBEAT_TIMEOUT_MS);
      if (!isActive || activeRoomRef.current?.code !== activeRoom.code || isTransitioningRef.current) {
        return;
      }

      switch (result.kind) {
        case 'ok': {
          const error = await desktopApi.updateNativeRoomToken(result.token);
          if (error !== null) {
            console.warn('Failed to refresh the presenter token:', error);
          }
          return;
        }
        case 'retry':
          console.warn(`Presenter heartbeat failed, retrying: ${result.reason}`);
          return;
        case 'unsupported':
          return;
        case 'closed':
          notify('error', 'Room closed', 'The server closed this room. Create a new room to continue.');
          await desktopApi.disconnectNativeRoom();
          clearRoom();
          onDisconnect?.();
      }
    };

    const interval = setInterval(() => void beat(), HEARTBEAT_MS);
    return () => {
      isActive = false;
      clearInterval(interval);
    };
  }, [activeRoom, clearRoom, onDisconnect]);

  return {
    closeRoom,
    connectionState,
    closeRoomBestEffort,
    connectPreparedRoom,
    createRoom,
    discardPreparedRoom,
    isClosingRoom,
    isCreatingRoom,
    prepareRoom,
    roomCode: activeRoom?.code ?? '',
    roomEndpoint: activeRoom?.apiEndpoint ?? null,
    shareUrl: activeRoom?.shareUrl ?? '',
    spectatorCount,
  };
};
