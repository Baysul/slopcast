import { useCallback, useEffect, useRef, useState } from 'react';
import { desktopApi } from '../api/desktop';
import { notify, primeAudioContext } from '../lib/toast';

const POLL_MS = 1000;

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

async function readErrorMessage(response: Response, fallback: string): Promise<string> {
  try {
    // SAFETY: room API errors use this colocated JSON contract.
    const body = (await response.json()) as { error?: string };
    if (body.error) return body.error;
  } catch {
    return fallback;
  }

  return fallback;
}

async function fetchSpectatorCount(room: PreparedRoom): Promise<number | null> {
  try {
    const response = await fetch(`${room.apiEndpoint}/api/rooms/${room.code}/spectators`);
    if (!response.ok) return null;

    // SAFETY: the spectator-count endpoint uses this colocated JSON contract.
    const body = (await response.json()) as { count?: number };
    if (body.count == null || !Number.isFinite(body.count)) return null;

    return body.count;
  } catch (error) {
    console.warn('Transient spectator count failure:', error);
    return null;
  }
}

async function requestRoom(apiEndpoint: string): Promise<PreparedRoom> {
  const response = await fetch(`${apiEndpoint}/api/rooms`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'X-Client-Origin': 'desktop' },
  });
  if (!response.ok) {
    throw new Error(await readErrorMessage(response, `Server returned ${response.status}`));
  }

  // SAFETY: the successful room endpoint uses this colocated JSON contract.
  const body = (await response.json()) as Omit<PreparedRoom, 'apiEndpoint'>;

  return { apiEndpoint, ...body };
}

async function deleteRoom(room: PreparedRoom, keepalive = false): Promise<RoomOperationResult> {
  try {
    const response = await fetch(`${room.apiEndpoint}/api/rooms/${room.code}`, {
      method: 'DELETE',
      headers: { 'X-Room-Close-Key': room.closeKey },
      keepalive,
    });
    if (response.ok || response.status === 404) return { ok: true };

    return { ok: false, error: await readErrorMessage(response, `Server returned ${response.status}`) };
  } catch {
    return { ok: false, error: 'The API could not be reached. Check the endpoint and try again.' };
  }
}

export interface UseNativeRoomOptions {
  apiEndpoint: string;
  livekitUrl: string;
  onDisconnect?: () => void;
}

export interface UseNativeRoomReturn {
  roomCode: string;
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

export function useNativeRoom({ apiEndpoint, livekitUrl, onDisconnect }: UseNativeRoomOptions): UseNativeRoomReturn {
  const [activeRoom, setActiveRoom] = useState<PreparedRoom | null>(null);
  const [spectatorCount, setSpectatorCount] = useState(0);
  const [isCreatingRoom, setIsCreatingRoom] = useState(false);
  const [isClosingRoom, setIsClosingRoom] = useState(false);
  const activeRoomRef = useRef<PreparedRoom | null>(null);
  const sawConnectedRef = useRef(false);
  const isTransitioningRef = useRef(false);

  const clearRoom = useCallback((): void => {
    activeRoomRef.current = null;
    sawConnectedRef.current = false;
    setActiveRoom(null);
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
      if (error !== null) return { ok: false, error };

      activeRoomRef.current = room;
      sawConnectedRef.current = false;
      setActiveRoom(room);
      setSpectatorCount(0);
      return { ok: true };
    },
    [livekitUrl],
  );

  const discardPreparedRoom = useCallback(async (room: PreparedRoom): Promise<void> => {
    const result = await deleteRoom(room);
    if (!result.ok) console.warn(`Failed to discard prepared room ${room.code}: ${result.error}`);
  }, []);

  const createRoom = useCallback(async (): Promise<boolean> => {
    if (isCreatingRoom || activeRoomRef.current) return false;
    const room = await prepareRoom(apiEndpoint);
    if (!room) return false;

    const result = await connectPreparedRoom(room);
    if (result.ok) return true;

    await discardPreparedRoom(room);
    notify('error', 'Room connection failed', result.error ?? 'The LiveKit room could not be reached.');
    return false;
  }, [apiEndpoint, connectPreparedRoom, discardPreparedRoom, isCreatingRoom, prepareRoom]);

  const closeRoom = useCallback(async (): Promise<RoomOperationResult> => {
    const room = activeRoomRef.current;
    if (!room) return { ok: true };
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
    if (room) void deleteRoom(room, true);
    void desktopApi.disconnectNativeRoom();
    clearRoom();
  }, [clearRoom]);

  useEffect(() => {
    return () => {
      const room = activeRoomRef.current;
      if (room) void deleteRoom(room, true);
      void desktopApi.disconnectNativeRoom();
    };
  }, []);

  useEffect(() => {
    if (!activeRoom) return;

    const poll = async (): Promise<void> => {
      const nextSpectatorCount = await fetchSpectatorCount(activeRoom);
      if (nextSpectatorCount !== null) setSpectatorCount(nextSpectatorCount);

      const connected = await desktopApi.isNativeRoomConnected();
      if (connected === true) sawConnectedRef.current = true;
      if (connected !== false || !sawConnectedRef.current || isTransitioningRef.current) return;

      const hasSession = await desktopApi.hasNativeRoomSession();
      if (hasSession || activeRoomRef.current?.code !== activeRoom.code) return;

      notify('error', 'Room disconnected', 'The connection to the room was lost. Create a new room to continue.');
      clearRoom();
      onDisconnect?.();
    };

    void poll();
    const interval = setInterval(() => void poll(), POLL_MS);
    return () => clearInterval(interval);
  }, [activeRoom, clearRoom, onDisconnect]);

  return {
    roomCode: activeRoom?.code ?? '',
    roomEndpoint: activeRoom?.apiEndpoint ?? null,
    shareUrl: activeRoom?.shareUrl ?? '',
    spectatorCount,
    isCreatingRoom,
    isClosingRoom,
    createRoom,
    prepareRoom,
    connectPreparedRoom,
    discardPreparedRoom,
    closeRoom,
    closeRoomBestEffort,
  };
}
