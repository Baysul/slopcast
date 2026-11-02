import { isJsonObject, normalizeLivekitUrl, readFiniteNumber, readString } from '@slopcast/shared-types';
import { ConnectionState, Room, RoomEvent, Track } from 'livekit-client';
import type { DisconnectReason, RemoteTrack } from 'livekit-client';
import { AlertCircle, ArrowLeft, Check, Copy, RefreshCw } from 'lucide-react';
import type React from 'react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { useNavigate, useParams } from 'react-router-dom';

import { Button } from '@/components/ui/button';

import { SpectatorBanner } from '../components/SpectatorBanner';
import { VideoPlayer } from '../components/VideoPlayer';
import {
  RECONNECT_ATTEMPTS_BEFORE_NOTICE,
  TokenRequestError,
  classifyDisconnect,
  isRoomGone,
  reconnectDelayMs,
} from '../lib/reconnect';

type StatusVariant = 'live' | 'disconnected' | 'info';

declare global {
  interface Window {
    slopcastReceiverStats?: () => Promise<RTCStatsReport | null>;
    slopcastConfig?: {
      apiEndpoint?: string;
      livekitUrl?: string;
    };
    __SLOPCAST_CONFIG__?: {
      apiEndpoint?: string;
      livekitUrl?: string;
    };
  }
}

const diagnosticEnabled = (): boolean => new URLSearchParams(window.location.search).has('diagnostics');

const StatusSignal: React.FC<{ variant: StatusVariant; children: React.ReactNode }> = ({ variant, children }) => {
  let dotClass = 'bg-muted-foreground/60';
  if (variant === 'live') {
    dotClass = 'bg-safelight motion-safe:animate-pulse';
  } else if (variant === 'info') {
    dotClass = 'bg-white/60';
  }
  const textClass = variant === 'live' ? 'text-safelight' : 'text-muted-foreground';

  return (
    <span className="inline-flex items-center gap-2 text-xs font-medium uppercase tracking-widest leading-none min-w-0">
      <span className={`size-1.5 rounded-full shrink-0 ${dotClass}`} aria-hidden="true" />
      <span className={`truncate min-w-0 ${textClass}`}>{children}</span>
    </span>
  );
};

const DECODER_STALL_THRESHOLD_MS = 8000;
const DECODER_STALL_CHECK_MS = 2000;
const CONNECT_TIMEOUT_MS = 20000;
const STREAM_END_GRACE_MS = 500;
const CONTROLS_IDLE_MS = 2500;

const logH264Sdp = (room: Room): void => {
  try {
    // SAFETY: this diagnostic reads LiveKit's current internal subscriber shape without mutating it.
    const sub = (
      room as { engine?: { pcManager?: { subscriber?: { getRemoteDescription: () => RTCSessionDescription | null } } } }
    ).engine?.pcManager?.subscriber;
    if (!sub) {
      return;
    }
    const desc = sub.getRemoteDescription();
    if (!desc) {
      return;
    }
    const h264Lines = desc.sdp
      .split('\n')
      .filter((line) => line.startsWith('a=fmtp:') && line.includes('profile-level-id'));
    for (const line of h264Lines) {
      console.log(`[SDP:recv] H264 remote fmtp: ${line}`);
    }
  } catch (error) {
    console.debug('[SDP:recv] failed to inspect remote description:', error);
  }
};

const collectExistingTracks = (room: Room): MediaStreamTrack[] => {
  const tracks: MediaStreamTrack[] = [];
  for (const participant of room.remoteParticipants.values()) {
    for (const pub of participant.trackPublications.values()) {
      if (pub.track?.mediaStreamTrack) {
        tracks.push(pub.track.mediaStreamTrack);
      }
    }
  }
  return tracks;
};

// Audio persists for the room lifetime; a live video publication means the presenter is sharing.
const hasRemoteVideo = (room: Room): boolean =>
  [...room.remoteParticipants.values()].some((participant) =>
    [...participant.videoTrackPublications.values()].some((publication) => publication.track !== undefined),
  );

const ensureManagedStream = (room: Room, managedStreamRef: React.RefObject<MediaStream | null>): MediaStream => {
  if (managedStreamRef.current) {
    return managedStreamRef.current;
  }
  const stream = new MediaStream();
  for (const existing of collectExistingTracks(room)) {
    if (existing.kind === 'audio') {
      stream.addTrack(existing);
    }
  }
  managedStreamRef.current = stream;
  return stream;
};

const endStream = (
  managedStreamRef: React.RefObject<MediaStream | null>,
  setMediaStream: (stream: MediaStream | null) => void,
  setConnectionStatus: (status: 'connecting' | 'live' | 'disconnected' | 'closed' | 'error') => void,
  setStatusText: (text: string) => void,
): void => {
  if (managedStreamRef.current) {
    const stream = managedStreamRef.current;
    managedStreamRef.current = null;
    for (const track of stream.getVideoTracks()) {
      track.stop();
    }
  }
  setMediaStream(null);
  setConnectionStatus('disconnected');
  setStatusText('Stream ended — waiting for presenter...');
};

const attachExistingTracks = (
  room: Room,
  managedStreamRef: React.RefObject<MediaStream | null>,
  setMediaStream: (stream: MediaStream | null) => void,
  setConnectionStatus: (status: 'connecting' | 'live' | 'disconnected' | 'closed' | 'error') => void,
  setStatusText: (text: string) => void,
): void => {
  const existingTracks = collectExistingTracks(room);
  if (existingTracks.length === 0) {
    if (room.state === ConnectionState.Connected) {
      setStatusText('Connected — waiting for stream...');
    }
    return;
  }
  managedStreamRef.current ??= new MediaStream();
  for (const track of existingTracks) {
    if (!managedStreamRef.current.getTracks().includes(track)) {
      managedStreamRef.current.addTrack(track);
    }
  }
  setMediaStream(managedStreamRef.current);
  if (existingTracks.some((track) => track.kind === 'video')) {
    setConnectionStatus('live');
    setStatusText('Live');
  } else {
    setConnectionStatus('connecting');
    setStatusText('Connected — waiting for stream...');
  }
};

const firstVideoReceiver = (room: Room): RTCRtpReceiver | undefined => {
  for (const participant of room.remoteParticipants.values()) {
    for (const publication of participant.videoTrackPublications.values()) {
      const track: unknown = publication.track;
      const receiver = isJsonObject(track) ? track.receiver : undefined;
      if (receiver instanceof RTCRtpReceiver) {
        return receiver;
      }
    }
  }
  return undefined;
};

interface VideoStatSnapshot {
  packetsReceived: number;
  framesReceived: number;
  framesDecoded: number;
  codecMime: string | null;
  decoderImpl: string | null;
}

const readVideoStats = async (receiver: RTCRtpReceiver): Promise<VideoStatSnapshot> => {
  const empty: VideoStatSnapshot = {
    codecMime: null,
    decoderImpl: null,
    framesDecoded: 0,
    framesReceived: 0,
    packetsReceived: 0,
  };
  let stats: RTCStatsReport | undefined = undefined;
  try {
    stats = await receiver.getStats();
  } catch (err) {
    console.warn('[Room] getStats failed:', err);
    return empty;
  }

  const snapshot: VideoStatSnapshot = { ...empty };
  for (const reportRaw of stats.values() as Iterable<unknown>) {
    if (
      isJsonObject(reportRaw) &&
      readString(reportRaw, 'type') === 'inbound-rtp' &&
      readString(reportRaw, 'kind') === 'video'
    ) {
      snapshot.packetsReceived = readFiniteNumber(reportRaw, 'packetsReceived') ?? 0;
      snapshot.framesReceived = readFiniteNumber(reportRaw, 'framesReceived') ?? 0;
      snapshot.framesDecoded = readFiniteNumber(reportRaw, 'framesDecoded') ?? 0;
      snapshot.decoderImpl = readString(reportRaw, 'decoderImplementation') ?? null;
      const codecId = readString(reportRaw, 'codecId');
      if (codecId !== undefined && codecId !== '') {
        const codecRaw: unknown = stats.get(codecId);
        snapshot.codecMime = isJsonObject(codecRaw) ? (readString(codecRaw, 'mimeType') ?? null) : null;
      }
    }
  }
  return snapshot;
};

const evaluateNegotiationFailure = (
  stats: VideoStatSnapshot,
  stallStartRef: React.RefObject<number>,
  decoderStalledRef: React.RefObject<boolean>,
  setDecoderStalled: (v: boolean) => void,
  setStalledCodec: (v: string | null) => void,
): boolean => {
  if (!(stats.packetsReceived > 0 && stats.codecMime === null)) {
    return false;
  }

  if (stallStartRef.current === 0) {
    stallStartRef.current = Date.now();
    console.warn(
      `[Room] Negotiation failure: packets=${stats.packetsReceived} framesReceived=${stats.framesReceived} ` +
        `framesDecoded=${stats.framesDecoded} codecMime=${stats.codecMime} impl=${stats.decoderImpl} — ` +
        `check offer caps / SDP fmtp`,
    );
  }

  if (Date.now() - stallStartRef.current >= DECODER_STALL_THRESHOLD_MS) {
    decoderStalledRef.current = true;
    setDecoderStalled(true);
    setStalledCodec(null);
    console.error(
      `[Room] SDP/caps negotiation failure confirmed after ${DECODER_STALL_THRESHOLD_MS}ms: ` +
        `packets=${stats.packetsReceived} but no inbound-rtp codec binding — ` +
        `sink sink_video_caps / supported_video_caps mismatch (H.264 stream-format/profile)`,
    );
  }
  return true;
};

const evaluateStall = (
  stats: VideoStatSnapshot,
  stallStartRef: React.RefObject<number>,
  decoderStalledRef: React.RefObject<boolean>,
  setDecoderStalled: (v: boolean) => void,
  setStalledCodec: (v: string | null) => void,
): void => {
  if (evaluateNegotiationFailure(stats, stallStartRef, decoderStalledRef, setDecoderStalled, setStalledCodec)) {
    return;
  }

  if (stats.packetsReceived > 0 && stats.framesDecoded === 0) {
    if (stallStartRef.current === 0) {
      stallStartRef.current = Date.now();
      console.warn(
        `[Room] Decoder stall suspected: packets=${stats.packetsReceived} framesReceived=${stats.framesReceived} ` +
          `framesDecoded=${stats.framesDecoded} codec=${stats.codecMime} impl=${stats.decoderImpl}`,
      );
    }

    if (Date.now() - stallStartRef.current >= DECODER_STALL_THRESHOLD_MS) {
      decoderStalledRef.current = true;
      setDecoderStalled(true);
      if (stats.codecMime !== null && stats.codecMime !== '') {
        setStalledCodec(stats.codecMime.replace(/^video\//iu, '').toUpperCase());
      }
      console.error(
        `[Room] Decoder stall confirmed after ${DECODER_STALL_THRESHOLD_MS}ms: ` +
          `packets=${stats.packetsReceived} framesReceived=${stats.framesReceived} ` +
          `framesDecoded=${stats.framesDecoded} codec=${stats.codecMime} impl=${stats.decoderImpl}`,
      );
    }
    return;
  }

  if (stats.framesDecoded > 0) {
    stallStartRef.current = 0;
    if (decoderStalledRef.current) {
      decoderStalledRef.current = false;
      setDecoderStalled(false);
      setStalledCodec(null);
    }
  }
};

export const RoomPage: React.FC = () => {
  const { roomId } = useParams<{ roomId: string }>();
  const navigate = useNavigate();

  const [connectionStatus, setConnectionStatus] = useState<'connecting' | 'live' | 'disconnected' | 'closed' | 'error'>(
    'connecting',
  );
  const [statusText, setStatusText] = useState('Connecting...');
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const [mediaStream, setMediaStream] = useState<MediaStream | null>(null);
  const [copied, setCopied] = useState(false);
  const [isFullscreen, setIsFullscreen] = useState(false);
  const [showControls, setShowControls] = useState(true);
  const [decoderStalled, setDecoderStalled] = useState(false);
  const [stalledCodec, setStalledCodec] = useState<string | null>(null);

  const resetConnectionView = (): void => {
    setConnectionStatus('connecting');
    setStatusText('Connecting...');
    setErrorMsg(null);
    setMediaStream(null);
    setDecoderStalled(false);
    setStalledCodec(null);
  };

  const [connectionRoomId, setConnectionRoomId] = useState(roomId);
  if (connectionRoomId !== roomId) {
    // React's "adjust state when a prop changes" pattern: a new room resets the connection view
    // During render instead of in an effect that would otherwise trigger a second render pass.
    setConnectionRoomId(roomId);
    resetConnectionView();
  }

  const roomRef = useRef<Room | null>(null);
  const connectGenRef = useRef(0);
  const managedStreamRef = useRef<MediaStream | null>(null);
  const stallCheckRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const stallStartRef = useRef<number>(0);
  const stallGenerationRef = useRef(0);
  const decoderStalledRef = useRef(false);
  const idleTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const controlsLastActiveAtRef = useRef(0);
  const areControlsVisibleRef = useRef<boolean>(true);
  const streamEndTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const connectFailedRef = useRef<boolean>(false);
  const rejoinTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const rejoinAttemptRef = useRef(0);
  const hasJoinedRef = useRef(false);
  const initializeConnectionRef = useRef<() => void>(() => undefined);

  const clearRejoinTimer = useCallback((): void => {
    if (rejoinTimerRef.current !== null) {
      clearTimeout(rejoinTimerRef.current);
      rejoinTimerRef.current = null;
    }
  }, []);

  // Rejoin with a fresh token after LiveKit gave up on its own resume, or after a failed join.
  // The view keeps its last state (and the presenter's last frame) instead of resetting.
  // A failed join reports twice: LiveKit's Disconnected event (no cause) and then the rejected
  // Connect promise (with the cause). The first schedules; the second only fills in the notice.
  const scheduleRejoin = useCallback((cause: string | null): void => {
    if (rejoinTimerRef.current === null) {
      const attempt = rejoinAttemptRef.current;
      rejoinAttemptRef.current += 1;
      const delay = reconnectDelayMs(attempt);
      console.warn(`[Room] rejoining in ${delay}ms (attempt ${attempt + 1})`);
      setConnectionStatus('disconnected');
      setStatusText(hasJoinedRef.current ? 'Connection lost — reconnecting...' : 'Connecting...');
      rejoinTimerRef.current = setTimeout(() => {
        rejoinTimerRef.current = null;
        initializeConnectionRef.current();
      }, delay);
    }
    if (cause !== null) {
      console.warn(`[Room] join failed: ${cause}`);
      if (rejoinAttemptRef.current >= RECONNECT_ATTEMPTS_BEFORE_NOTICE) {
        setErrorMsg(`Still trying to connect: ${cause}`);
      }
    }
  }, []);

  const resetDecoderStall = useCallback((): void => {
    stallGenerationRef.current += 1;
    stallStartRef.current = 0;
    decoderStalledRef.current = false;
    setDecoderStalled(false);
    setStalledCodec(null);
  }, []);

  const resetControlsIdleTimer = useCallback(() => {
    controlsLastActiveAtRef.current = performance.now();
    if (!areControlsVisibleRef.current) {
      areControlsVisibleRef.current = true;
      setShowControls(true);
    }
    if (idleTimerRef.current !== null) {
      return;
    }

    const hideAfterIdle = (): void => {
      const remainingMs = CONTROLS_IDLE_MS - (performance.now() - controlsLastActiveAtRef.current);
      if (remainingMs > 0) {
        idleTimerRef.current = setTimeout(hideAfterIdle, remainingMs);
        return;
      }

      idleTimerRef.current = null;
      areControlsVisibleRef.current = false;
      setShowControls(false);
    };
    idleTimerRef.current = setTimeout(hideAfterIdle, CONTROLS_IDLE_MS);
  }, []);

  useEffect(() => {
    const handleFullscreenChange = () => {
      setIsFullscreen(Boolean(document.fullscreenElement));
      resetControlsIdleTimer();
    };

    document.addEventListener('fullscreenchange', handleFullscreenChange);
    return () => {
      document.removeEventListener('fullscreenchange', handleFullscreenChange);
    };
  }, [resetControlsIdleTimer]);

  useEffect(() => {
    const handleActivity = () => {
      resetControlsIdleTimer();
    };

    resetControlsIdleTimer();
    window.addEventListener('pointermove', handleActivity);
    window.addEventListener('pointerdown', handleActivity);
    window.addEventListener('touchstart', handleActivity);
    window.addEventListener('keydown', handleActivity);

    return () => {
      window.removeEventListener('pointermove', handleActivity);
      window.removeEventListener('pointerdown', handleActivity);
      window.removeEventListener('touchstart', handleActivity);
      window.removeEventListener('keydown', handleActivity);
      if (idleTimerRef.current !== null) {
        clearTimeout(idleTimerRef.current);
      }
    };
  }, [resetControlsIdleTimer]);

  const copyLink = () => {
    navigator.clipboard.writeText(window.location.href).catch((err) => {
      console.warn('[Room] copy link failed:', err);
    });
    setCopied(true);
    setTimeout(() => {
      setCopied(false);
    }, 2000);
  };

  const initializeConnection = useCallback(() => {
    if (roomId === undefined || roomId === '') {
      return;
    }

    const gen = ++connectGenRef.current;
    const isStale = () => connectGenRef.current !== gen;

    clearRejoinTimer();
    connectFailedRef.current = false;
    // The initial render, room-ID change, or reconnect action resets the view.
    // Here only invalidate outstanding decoder observations before connecting.
    stallGenerationRef.current += 1;
    stallStartRef.current = 0;
    decoderStalledRef.current = false;

    if (stallCheckRef.current !== null) {
      clearInterval(stallCheckRef.current);
      stallCheckRef.current = null;
    }
    if (streamEndTimerRef.current !== null) {
      clearTimeout(streamEndTimerRef.current);
      streamEndTimerRef.current = null;
    }

    if (managedStreamRef.current) {
      managedStreamRef.current.getTracks().forEach((t) => {
        t.stop();
      });
      managedStreamRef.current = null;
    }
    if (roomRef.current) {
      roomRef.current.removeAllListeners();
      void roomRef.current.disconnect();
      roomRef.current = null;
    }

    const room = new Room({ adaptiveStream: false });
    roomRef.current = room;

    room.on(RoomEvent.TrackSubscribed, (track: RemoteTrack) => {
      if (isStale()) {
        return;
      }
      if (streamEndTimerRef.current !== null) {
        clearTimeout(streamEndTimerRef.current);
        streamEndTimerRef.current = null;
      }
      if (track.kind === Track.Kind.Video) {
        resetDecoderStall();
      }
      const stream = ensureManagedStream(room, managedStreamRef);
      if (!stream.getTracks().includes(track.mediaStreamTrack)) {
        stream.addTrack(track.mediaStreamTrack);
      }
      setMediaStream(stream);
      if (track.kind === Track.Kind.Video) {
        setConnectionStatus('live');
        setStatusText('Live');
        logH264Sdp(room);
      } else if (!hasRemoteVideo(room)) {
        setConnectionStatus('connecting');
        setStatusText('Connected — waiting for stream...');
      }
    });

    room.on(RoomEvent.TrackUnsubscribed, (track: RemoteTrack) => {
      if (isStale()) {
        return;
      }
      if (track.kind !== Track.Kind.Video) {
        return;
      }
      resetDecoderStall();
      if (managedStreamRef.current) {
        managedStreamRef.current.removeTrack(track.mediaStreamTrack);
      }
      if (streamEndTimerRef.current !== null) {
        clearTimeout(streamEndTimerRef.current);
      }
      streamEndTimerRef.current = setTimeout(() => {
        streamEndTimerRef.current = null;
        if (isStale()) {
          return;
        }
        if (hasRemoteVideo(room)) {
          return;
        }
        if (room.remoteParticipants.size === 0) {
          return;
        }
        endStream(managedStreamRef, setMediaStream, setConnectionStatus, setStatusText);
      }, STREAM_END_GRACE_MS);
    });

    room.on(RoomEvent.ConnectionStateChanged, (state: ConnectionState) => {
      if (isStale()) {
        return;
      }
      switch (state) {
        case ConnectionState.Connected:
          if (managedStreamRef.current && managedStreamRef.current.getVideoTracks().length > 0) {
            setConnectionStatus('live');
            setStatusText('Live');
          } else {
            setStatusText('Connected — waiting for stream...');
          }
          break;
        case ConnectionState.Reconnecting:
          setStatusText('Reconnecting...');
          break;
        case ConnectionState.Disconnected:
          // The Disconnected event carries the reason and decides between rejoining and ending.
          resetDecoderStall();
          break;
        case ConnectionState.Connecting:
        case ConnectionState.SignalReconnecting:
          break;
      }
    });

    room.on(RoomEvent.Disconnected, (reason?: DisconnectReason) => {
      if (isStale()) {
        return;
      }
      if (connectFailedRef.current) {
        return;
      }
      resetDecoderStall();
      switch (classifyDisconnect(reason)) {
        case 'ignore':
          return;
        case 'ended':
          setConnectionStatus('closed');
          setStatusText('Stream ended');
          return;
        case 'rejoin':
          scheduleRejoin(null);
      }
    });

    room.on(RoomEvent.TrackPublished, () => {
      if (isStale()) {
        return;
      }
      const unsupported = [...room.remoteParticipants.values()]
        .flatMap((p) => [...p.videoTrackPublications.values()])
        .find((pub) => {
          if (pub.track) {
            return false;
          }
          const mime = pub.mimeType?.toUpperCase();
          if (mime === undefined || mime === '') {
            return false;
          }
          const receiverCodecs =
            RTCRtpReceiver.getCapabilities('video')?.codecs.map((codec) => codec.mimeType.toUpperCase()) ?? [];
          return !receiverCodecs.some((m) => m.includes(mime.replace(/^VIDEO\//u, '')));
        });
      if (!unsupported) {
        return;
      }
      const codec = unsupported.mimeType?.replace(/^video\//iu, '').toUpperCase();
      resetDecoderStall();
      setConnectionStatus('error');
      setStatusText(`This browser cannot decode ${codec} video`);
      setErrorMsg(
        `The presenter is streaming ${codec}, which this browser does not support. ` +
          'Try Chrome or Edge with HEVC hardware support.',
      );
    });

    room.on(RoomEvent.ParticipantDisconnected, () => {
      if (isStale()) {
        return;
      }
      // The presenter app rejoins on its own after network trouble; the server ends the room
      // (and this page) if they do not come back within its grace period.
      if (room.remoteParticipants.size === 0) {
        resetDecoderStall();
        setConnectionStatus('disconnected');
        setStatusText('Presenter disconnected — waiting for them to reconnect...');
      }
    });

    // Older deployments may still inject the legacy global.
    const { slopcastConfig, __SLOPCAST_CONFIG__: legacyConfig } = window;
    const config = slopcastConfig ?? legacyConfig;
    const apiEndpoint = config?.apiEndpoint;
    const injectedLivekitUrl = config?.livekitUrl;

    let baseUrl = `${window.location.protocol}//${window.location.hostname}:3001`;
    if (apiEndpoint !== undefined && apiEndpoint !== '') {
      baseUrl = apiEndpoint;
    } else if (injectedLivekitUrl !== undefined && injectedLivekitUrl !== '') {
      baseUrl = injectedLivekitUrl.replace(/^ws(?<secure>s?):\/\//u, 'http$<secure>://');
    }

    const getToken = async (): Promise<{ token: string; livekitUrl: string }> => {
      const controller = new AbortController();
      const timeout = setTimeout(() => {
        controller.abort();
      }, 15000);
      try {
        const res = await fetch(`${baseUrl}/api/rooms/${roomId}/token`, {
          signal: controller.signal,
        });
        if (!res.ok) {
          const errData: unknown = await res.json().catch(() => ({}));
          const message = isJsonObject(errData) ? readString(errData, 'error') : undefined;
          throw new TokenRequestError(
            message !== undefined && message !== '' ? message : `Failed to fetch spectator token (${res.status})`,
            res.status,
          );
        }
        const data: unknown = await res.json();
        if (!isJsonObject(data)) {
          throw new Error('Spectator token response was not a JSON object');
        }
        const token = readString(data, 'token');
        const livekitUrl = readString(data, 'livekitUrl');
        if (token === undefined || livekitUrl === undefined) {
          throw new Error('Spectator token response is missing token or livekitUrl');
        }
        return { livekitUrl, token };
      } finally {
        clearTimeout(timeout);
      }
    };

    getToken()
      .then(async ({ token, livekitUrl }) => {
        if (isStale()) {
          return;
        }
        const livekitUrlForClient = normalizeLivekitUrl(
          [livekitUrl, injectedLivekitUrl].find((url) => url !== undefined && url !== '') ??
            `ws://${window.location.hostname}:7880`,
          window.location.protocol === 'https:',
        );
        try {
          let timeout: ReturnType<typeof setTimeout> | undefined = undefined;
          const connectPromise = room.connect(livekitUrlForClient, token);
          const timeoutPromise = new Promise<never>((_, reject) => {
            timeout = setTimeout(() => {
              reject(
                new Error(
                  `timed out connecting to ${livekitUrlForClient} after ${CONNECT_TIMEOUT_MS / 1000}s — is the LiveKit server running?`,
                ),
              );
            }, CONNECT_TIMEOUT_MS);
          });
          try {
            await Promise.race([connectPromise, timeoutPromise]);
          } finally {
            clearTimeout(timeout);
          }
        } catch (err) {
          void room.disconnect().catch(() => undefined);
          throw err;
        }
        if (isStale()) {
          void room.disconnect();
          return;
        }
        hasJoinedRef.current = true;
        rejoinAttemptRef.current = 0;
        setErrorMsg(null);
        attachExistingTracks(room, managedStreamRef, setMediaStream, setConnectionStatus, setStatusText);
      })
      .catch((err) => {
        if (isStale()) {
          return;
        }
        connectFailedRef.current = true;
        const message = err instanceof Error ? err.message : String(err);
        if (isRoomGone(err)) {
          setConnectionStatus('closed');
          setStatusText(hasJoinedRef.current ? 'Stream ended' : 'Room not found');
          setErrorMsg(hasJoinedRef.current ? null : `${message}. Check the link with the presenter.`);
          return;
        }
        scheduleRejoin(message);
      });
  }, [clearRejoinTimer, roomId, resetDecoderStall, scheduleRejoin]);

  useEffect(() => {
    initializeConnectionRef.current = initializeConnection;
  }, [initializeConnection]);

  // Coming back online or to the foreground should not wait out a long backoff.
  useEffect(() => {
    const rejoinNow = (): void => {
      if (rejoinTimerRef.current === null || document.visibilityState === 'hidden') {
        return;
      }
      clearRejoinTimer();
      initializeConnectionRef.current();
    };
    window.addEventListener('online', rejoinNow);
    document.addEventListener('visibilitychange', rejoinNow);
    return () => {
      window.removeEventListener('online', rejoinNow);
      document.removeEventListener('visibilitychange', rejoinNow);
    };
  }, [clearRejoinTimer]);

  useEffect(() => {
    hasJoinedRef.current = false;
    rejoinAttemptRef.current = 0;
    initializeConnection();
    return () => {
      connectGenRef.current += 1;
      clearRejoinTimer();
      if (stallCheckRef.current !== null) {
        clearInterval(stallCheckRef.current);
        stallCheckRef.current = null;
      }
      if (streamEndTimerRef.current !== null) {
        clearTimeout(streamEndTimerRef.current);
        streamEndTimerRef.current = null;
      }
      if (managedStreamRef.current) {
        managedStreamRef.current.getTracks().forEach((t) => {
          t.stop();
        });
        managedStreamRef.current = null;
      }
      if (roomRef.current) {
        roomRef.current.removeAllListeners();
        void roomRef.current.disconnect();
        roomRef.current = null;
      }
    };
  }, [clearRejoinTimer, initializeConnection]);

  const isDecoderStalled = connectionStatus === 'live' && decoderStalled;
  const stalledCodecName = connectionStatus === 'live' ? stalledCodec : null;

  useEffect(() => {
    if (connectionStatus !== 'live') {
      if (stallCheckRef.current !== null) {
        clearInterval(stallCheckRef.current);
        stallCheckRef.current = null;
      }
      stallStartRef.current = 0;
      return undefined;
    }

    stallStartRef.current = 0;
    let isActive = true;
    const checkDecoder = async (): Promise<void> => {
      const room = roomRef.current;
      if (!room) {
        return;
      }
      const receiver = firstVideoReceiver(room);
      if (!receiver) {
        return;
      }
      const generation = stallGenerationRef.current;
      const stats = await readVideoStats(receiver);
      if (!isActive || roomRef.current !== room || stallGenerationRef.current !== generation) {
        return;
      }
      evaluateStall(stats, stallStartRef, decoderStalledRef, setDecoderStalled, setStalledCodec);
    };

    stallCheckRef.current = setInterval(() => {
      void checkDecoder();
    }, DECODER_STALL_CHECK_MS);

    return () => {
      isActive = false;
      if (stallCheckRef.current !== null) {
        clearInterval(stallCheckRef.current);
        stallCheckRef.current = null;
      }
    };
  }, [connectionStatus]);

  const handleResync = () => {
    rejoinAttemptRef.current = 0;
    resetConnectionView();
    initializeConnection();
  };

  const getStatsFn = useCallback(async (): Promise<RTCStatsReport | null> => {
    const room = roomRef.current;
    if (!room) {
      return null;
    }
    const receiver = firstVideoReceiver(room);
    if (!receiver) {
      return null;
    }
    try {
      return await receiver.getStats();
    } catch (err) {
      console.warn('[Room] getStats failed:', err);
      return null;
    }
  }, []);

  useEffect(() => {
    if (!diagnosticEnabled()) {
      return undefined;
    }
    window.slopcastReceiverStats = getStatsFn;
    return () => {
      delete window.slopcastReceiverStats;
    };
  }, [getStatsFn]);

  const statusVariant = (): StatusVariant => {
    if (connectionStatus === 'live') {
      return 'live';
    }
    if (connectionStatus === 'connecting') {
      return 'info';
    }
    return 'disconnected';
  };
  const variant = statusVariant();
  const CopyIcon = copied ? Check : Copy;

  const headerFadeClass = showControls ? 'opacity-100' : 'opacity-0 pointer-events-none';

  return (
    <div className="min-h-screen bg-background text-foreground relative">
      <div className="absolute inset-0 z-10">
        <VideoPlayer
          mediaStream={mediaStream}
          isLive={connectionStatus === 'live'}
          statusText={statusText}
          onResync={handleResync}
          fullBleed
          getStatsFn={getStatsFn}
          decoderStalled={isDecoderStalled}
          stalledCodec={stalledCodecName}
          isFullscreen={isFullscreen}
          showControls={showControls}
          onControlsActivity={resetControlsIdleTimer}
        />
      </div>

      <div
        className={`fixed top-0 inset-x-0 bg-gradient-to-b from-black/60 to-transparent px-4 pt-3 pb-8 z-30 pointer-events-none transition-opacity duration-300 ${headerFadeClass}`}
      >
        <div className="flex items-center justify-between pointer-events-none gap-3">
          <div className="flex items-center gap-2 min-w-0 pointer-events-auto">
            <button
              type="button"
              onClick={() => {
                void navigate('/');
              }}
              aria-label="Leave room"
              title="Leave room"
              className="p-2 text-muted-foreground hover:text-foreground hover:bg-white/10 rounded-lg transition-colors duration-200 shrink-0 cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-black"
            >
              <ArrowLeft className="w-5 h-5" />
            </button>
            <output
              aria-live="polite"
              className="inline-flex items-center min-w-0 max-w-[60vw] sm:max-w-[320px] shrink"
            >
              <StatusSignal variant={variant}>{statusText}</StatusSignal>
            </output>
          </div>
          <button
            type="button"
            onClick={copyLink}
            aria-label={copied ? 'Link copied' : 'Copy room link'}
            title="Copy room link"
            className="p-2 text-muted-foreground hover:text-foreground hover:bg-white/10 rounded-lg transition-colors duration-200 shrink-0 cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-black pointer-events-auto"
          >
            <CopyIcon className="w-4 h-4" />
          </button>
        </div>
      </div>

      <div
        className={`fixed bottom-4 left-4 z-30 pointer-events-none transition-opacity duration-300 ${headerFadeClass}`}
      >
        <SpectatorBanner compact />
      </div>

      {errorMsg !== null && errorMsg !== '' && (
        <div className="fixed bottom-6 inset-x-0 flex justify-center px-4 z-30 pointer-events-none">
          <div
            role="alert"
            className="bg-black/80 border border-destructive/25 text-destructive px-4 py-3 rounded-lg flex items-center gap-3 flex-wrap max-w-[90vw] sm:max-w-md backdrop-blur-md pointer-events-auto shadow-lg"
          >
            <AlertCircle className="w-4 h-4 shrink-0" aria-hidden="true" />
            <span className="text-xs font-medium min-w-0 flex-1">{errorMsg}</span>
            <Button size="sm" variant="outline" onClick={handleResync} className="text-xs ml-2 border-destructive/20">
              <RefreshCw className="w-3 h-3" />
              <span>Retry</span>
            </Button>
          </div>
        </div>
      )}
    </div>
  );
};
