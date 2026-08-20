import { normalizeLivekitUrl } from '@slopcast/shared-types';
import { ConnectionState, type RemoteTrack, Room, RoomEvent } from 'livekit-client';
import { AlertCircle, ArrowLeft, Check, Copy, RefreshCw } from 'lucide-react';
import type React from 'react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { useNavigate, useParams } from 'react-router-dom';
import { Button } from '@/components/ui/button';
import { SpectatorBanner } from '../components/SpectatorBanner';
import { VideoPlayer } from '../components/VideoPlayer';

type StatusVariant = 'live' | 'disconnected' | 'info';

declare global {
  interface Window {
    __slopcastReceiverStats?: () => Promise<RTCStatsReport | null>;
  }
}

const diagnosticEnabled = (): boolean => new URLSearchParams(window.location.search).has('diagnostics');

// Distilled status signal, matching the desktop titlebar convention: a bare
// dot + uppercase word, no pill or background. The video owns the viewport.
const StatusSignal: React.FC<{ variant: StatusVariant; children: React.ReactNode }> = ({ variant, children }) => {
  let dotClass: string;
  if (variant === 'live') {
    dotClass = 'bg-safelight motion-safe:animate-pulse';
  } else if (variant === 'info') {
    dotClass = 'bg-white/60';
  } else {
    dotClass = 'bg-muted-foreground/60';
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
// Belt-and-braces guard around room.connect: livekit-client times out its
// own signal handshake, but the status badge must never sit on "Connecting..."
// indefinitely if a future bump regresses that.
const CONNECT_TIMEOUT_MS = 20000;
// A track re-publish (settings change) unsubscribes then re-subscribes the
// video track within a beat; wait before declaring the stream ended.
const STREAM_END_GRACE_MS = 500;

const logH264Sdp = (room: Room): void => {
  try {
    const sub = (
      room as { engine?: { pcManager?: { subscriber?: { getRemoteDescription(): RTCSessionDescription | null } } } }
    ).engine?.pcManager?.subscriber;
    if (!sub) return;
    const desc = sub.getRemoteDescription();
    if (!desc) return;
    const h264Lines = desc.sdp
      .split('\n')
      .filter((line) => line.startsWith('a=fmtp:') && line.includes('profile-level-id'));
    for (const line of h264Lines) {
      console.log(`[SDP:recv] H264 remote fmtp: ${line}`);
    }
  } catch {
    /* diagnostic-only */
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

// Stream liveness is defined by video publications: the presenter publishes
// an audio track for the whole room lifetime, so audio alone means "presenter
// connected, not sharing" — never "live". Only publications with a live track
// count: when the SFU closes a downtrack, TrackUnsubscribed fires from the
// track-ended path *before* the participant-update cleanup removes the stale
// publication, so the maps lag the event.
const hasRemoteVideo = (room: Room): boolean =>
  [...room.remoteParticipants.values()].some((participant) =>
    [...participant.videoTrackPublications.values()].some((publication) => publication.track != null),
  );

// Ensures a managed stream exists. Fresh streams re-attach every
// still-subscribed audio track: the presenter's audio publication survives a
// stop/restart cycle within the same room, so it never re-subscribes.
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
    // Only the unsubscribed video tracks are dead; the presenter's audio
    // track stays subscribed in the room and must not be stopped — it is
    // re-attached to the next stream when the presenter shares again.
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
  if (!managedStreamRef.current) {
    managedStreamRef.current = new MediaStream();
  }
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
    // Only the room-lifetime audio track is up: the presenter is connected
    // but not sharing yet.
    setConnectionStatus('connecting');
    setStatusText('Connected — waiting for stream...');
  }
};

const firstVideoReceiver = (room: Room): RTCRtpReceiver | undefined => {
  for (const participant of room.remoteParticipants.values()) {
    for (const publication of participant.videoTrackPublications.values()) {
      const receiver = (publication.track as { receiver?: RTCRtpReceiver } | undefined)?.receiver;
      if (receiver) return receiver;
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
    packetsReceived: 0,
    framesReceived: 0,
    framesDecoded: 0,
    codecMime: null,
    decoderImpl: null,
  };
  let stats: RTCStatsReport;
  try {
    stats = await receiver.getStats();
  } catch (err) {
    console.warn('[Room] getStats failed:', err);
    return empty;
  }

  const snapshot: VideoStatSnapshot = { ...empty };
  for (const reportRaw of stats.values()) {
    const report = reportRaw as {
      type: string;
      kind?: string;
      codecId?: string;
      packetsReceived?: number;
      framesReceived?: number;
      framesDecoded?: number;
      decoderImplementation?: string;
    };
    if (report.type === 'inbound-rtp' && report.kind === 'video') {
      snapshot.packetsReceived = report.packetsReceived ?? 0;
      snapshot.framesReceived = report.framesReceived ?? 0;
      snapshot.framesDecoded = report.framesDecoded ?? 0;
      snapshot.decoderImpl = report.decoderImplementation ?? null;
      if (report.codecId) {
        const codec = stats.get(report.codecId) as { mimeType?: string } | undefined;
        snapshot.codecMime = codec?.mimeType ?? null;
      }
    }
  }
  return snapshot;
};

const evaluateNegotiationFailure = (
  stats: VideoStatSnapshot,
  stallStartRef: React.RefObject<number>,
  setDecoderStalled: (v: boolean) => void,
  setStalledCodec: (v: string | null) => void,
): boolean => {
  if (!(stats.packetsReceived > 0 && stats.codecMime == null)) return false;

  if (stallStartRef.current === 0) {
    stallStartRef.current = Date.now();
    console.warn(
      `[Room] Negotiation failure: packets=${stats.packetsReceived} framesReceived=${stats.framesReceived} ` +
        `framesDecoded=${stats.framesDecoded} codecMime=${stats.codecMime} impl=${stats.decoderImpl} — ` +
        `check offer caps / SDP fmtp`,
    );
  }

  if (Date.now() - stallStartRef.current >= DECODER_STALL_THRESHOLD_MS) {
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
  if (evaluateNegotiationFailure(stats, stallStartRef, setDecoderStalled, setStalledCodec)) return;

  if (stats.packetsReceived > 0 && stats.framesDecoded === 0) {
    if (stallStartRef.current === 0) {
      stallStartRef.current = Date.now();
      console.warn(
        `[Room] Decoder stall suspected: packets=${stats.packetsReceived} framesReceived=${stats.framesReceived} ` +
          `framesDecoded=${stats.framesDecoded} codec=${stats.codecMime} impl=${stats.decoderImpl}`,
      );
    }

    if (Date.now() - stallStartRef.current >= DECODER_STALL_THRESHOLD_MS) {
      setDecoderStalled(true);
      if (stats.codecMime) {
        setStalledCodec(stats.codecMime.replace(/^video\//i, '').toUpperCase());
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

  const [participantCount, setParticipantCount] = useState(0);
  const [mediaStream, setMediaStream] = useState<MediaStream | null>(null);
  const [copied, setCopied] = useState(false);
  const [isFullscreen, setIsFullscreen] = useState(false);
  const [showFullscreenControls, setShowFullscreenControls] = useState(true);
  const [decoderStalled, setDecoderStalled] = useState(false);
  const [stalledCodec, setStalledCodec] = useState<string | null>(null);

  const roomRef = useRef<Room | null>(null);
  const connectGenRef = useRef(0);
  const managedStreamRef = useRef<MediaStream | null>(null);
  const stallCheckRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const stallStartRef = useRef<number>(0);
  const idleTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const streamEndTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Set when a connection attempt fails; guards the badge state from being
  // overwritten by the tear-down events (Disconnected) that follow.
  const connectFailedRef = useRef(false);

  const resetIdleTimer = useCallback(() => {
    setShowFullscreenControls(true);
    if (idleTimerRef.current) {
      clearTimeout(idleTimerRef.current);
    }
    idleTimerRef.current = setTimeout(() => {
      setShowFullscreenControls(false);
    }, 2500);
  }, []);

  useEffect(() => {
    const handleFullscreenChange = () => {
      const fs = !!document.fullscreenElement;
      setIsFullscreen(fs);
      if (!fs) {
        setShowFullscreenControls(true);
        if (idleTimerRef.current) {
          clearTimeout(idleTimerRef.current);
          idleTimerRef.current = null;
        }
      } else {
        resetIdleTimer();
      }
    };

    document.addEventListener('fullscreenchange', handleFullscreenChange);
    return () => {
      document.removeEventListener('fullscreenchange', handleFullscreenChange);
      if (idleTimerRef.current) {
        clearTimeout(idleTimerRef.current);
      }
    };
  }, [resetIdleTimer]);

  useEffect(() => {
    if (!isFullscreen) return;

    const handleActivity = () => {
      resetIdleTimer();
    };

    window.addEventListener('pointermove', handleActivity);
    window.addEventListener('touchstart', handleActivity);
    window.addEventListener('keydown', handleActivity);

    return () => {
      window.removeEventListener('pointermove', handleActivity);
      window.removeEventListener('touchstart', handleActivity);
      window.removeEventListener('keydown', handleActivity);
    };
  }, [isFullscreen, resetIdleTimer]);

  const copyLink = () => {
    navigator.clipboard.writeText(window.location.href).catch((err) => {
      console.warn('[Room] copy link failed:', err);
    });
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const initializeConnection = useCallback(() => {
    if (!roomId) return;

    const gen = ++connectGenRef.current;
    const isStale = () => connectGenRef.current !== gen;

    setConnectionStatus('connecting');
    setStatusText('Connecting...');
    setErrorMsg(null);
    setMediaStream(null);
    setParticipantCount(0);
    setDecoderStalled(false);
    setStalledCodec(null);
    connectFailedRef.current = false;

    if (stallCheckRef.current) {
      clearInterval(stallCheckRef.current);
      stallCheckRef.current = null;
    }
    stallStartRef.current = 0;
    if (streamEndTimerRef.current) {
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
      roomRef.current.disconnect();
      roomRef.current = null;
    }

    // VideoPlayer attaches the underlying MediaStreamTrack rather than using
    // RemoteVideoTrack.attach(), so LiveKit cannot receive the visibility and
    // dimensions required by adaptive streaming.
    const room = new Room({ adaptiveStream: false });
    roomRef.current = room;

    room.on(RoomEvent.TrackSubscribed, (track: RemoteTrack) => {
      if (isStale()) return;
      if (streamEndTimerRef.current) {
        clearTimeout(streamEndTimerRef.current);
        streamEndTimerRef.current = null;
      }
      const stream = ensureManagedStream(room, managedStreamRef);
      if (!stream.getTracks().includes(track.mediaStreamTrack)) {
        stream.addTrack(track.mediaStreamTrack);
      }
      // Reuse the same MediaStream object identity: mutating it in place keeps
      // VideoPlayer's srcObject binding stable, so playback is never restarted
      // by a track subscribe/unsubscribe.
      setMediaStream(stream);
      if (track.kind === 'video') {
        setConnectionStatus('live');
        setStatusText('Live');
        logH264Sdp(room);
      } else if (!hasRemoteVideo(room)) {
        // Audio alone means the presenter is connected but not sharing. Never
        // downgrade an already-live stream.
        setConnectionStatus('connecting');
        setStatusText('Connected — waiting for stream...');
      }
    });

    room.on(RoomEvent.TrackUnsubscribed, (track: RemoteTrack) => {
      if (isStale()) return;
      if (track.kind !== 'video') return;
      if (managedStreamRef.current) {
        managedStreamRef.current.removeTrack(track.mediaStreamTrack);
      }
      // Do NOT gate on the publication maps here: the event fires from the
      // track-ended path while the stale publication is still present (the
      // participant-update cleanup lags). Arm the grace timer instead and let
      // it re-check the settled state. A re-publish (settings change) and the
      // presenter leaving both pass through here; give a re-subscribe or the
      // participant teardown a beat to arrive before declaring the stream
      // ended.
      if (streamEndTimerRef.current) {
        clearTimeout(streamEndTimerRef.current);
      }
      streamEndTimerRef.current = setTimeout(() => {
        streamEndTimerRef.current = null;
        if (isStale()) return;
        if (hasRemoteVideo(room)) return;
        if (room.remoteParticipants.size === 0) return; // ParticipantDisconnected owns this state
        endStream(managedStreamRef, setMediaStream, setConnectionStatus, setStatusText);
      }, STREAM_END_GRACE_MS);
    });

    room.on(RoomEvent.ConnectionStateChanged, (state: ConnectionState) => {
      if (isStale()) return;
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
          if (connectFailedRef.current) return;
          setConnectionStatus('disconnected');
          setStatusText('Connection lost');
          break;
        default:
          break;
      }
    });

    room.on(RoomEvent.Disconnected, () => {
      if (isStale()) return;
      if (connectFailedRef.current) return;
      setConnectionStatus('closed');
      setStatusText('Room closed');
    });

    room.on(RoomEvent.ParticipantConnected, () => {
      if (isStale()) return;
      setParticipantCount(room.remoteParticipants.size);
    });

    // A video publication this browser cannot subscribe to (e.g. the
    // presenter publishing H.265 while this Chromium build has no HEVC
    // decoder) leaves the spectator stuck on "waiting for presenter" with no
    // error. Surface the codec gap instead of hanging silently.
    room.on(RoomEvent.TrackPublished, () => {
      if (isStale()) return;
      const unsupported = [...room.remoteParticipants.values()]
        .flatMap((p) => [...p.videoTrackPublications.values()])
        .find((pub) => {
          if (pub.track) return false;
          const mime = pub.mimeType?.toUpperCase();
          if (!mime) return false;
          const receiverCodecs =
            typeof RTCRtpReceiver !== 'undefined' && RTCRtpReceiver.getCapabilities
              ? (RTCRtpReceiver.getCapabilities('video')?.codecs.map((c) => c.mimeType.toUpperCase()) ?? [])
              : [];
          return !receiverCodecs.some((m) => m.includes(mime.replace(/^VIDEO\//, '')));
        });
      if (!unsupported) return;
      const codec = unsupported.mimeType?.replace(/^video\//i, '').toUpperCase();
      setConnectionStatus('error');
      setStatusText(`This browser cannot decode ${codec} video`);
      setErrorMsg(
        `The presenter is streaming ${codec}, which this browser does not support. ` +
          'Try Chrome or Edge with HEVC hardware support.',
      );
    });

    room.on(RoomEvent.ParticipantDisconnected, () => {
      if (isStale()) return;
      const count = room.remoteParticipants.size;
      setParticipantCount(count);
      if (count === 0) {
        setConnectionStatus('closed');
        setStatusText('Presenter left');
      }
    });

    const apiEndpoint = (window as { __SLOPCAST_CONFIG__?: { apiEndpoint?: string } }).__SLOPCAST_CONFIG__?.apiEndpoint;
    const injectedLivekitUrl = (window as { __SLOPCAST_CONFIG__?: { livekitUrl?: string } }).__SLOPCAST_CONFIG__
      ?.livekitUrl;

    let baseUrl = `${window.location.protocol}//${window.location.hostname}:3001`;
    if (apiEndpoint) {
      baseUrl = apiEndpoint;
    } else if (injectedLivekitUrl) {
      baseUrl = injectedLivekitUrl.replace(/^ws(s?):\/\//, 'http$1://');
    }

    const getToken = async (): Promise<{ token: string; livekitUrl: string }> => {
      const controller = new AbortController();
      const timeout = setTimeout(() => controller.abort(), 15000);
      try {
        const res = await fetch(`${baseUrl}/api/rooms/${roomId}/token`, {
          signal: controller.signal,
        });
        if (!res.ok) {
          const errData = (await res.json().catch(() => ({}))) as { error?: string };
          throw new Error(errData.error || `Failed to fetch spectator token (${res.status})`);
        }
        const data = (await res.json()) as { token: string; livekitUrl: string };
        return data;
      } finally {
        clearTimeout(timeout);
      }
    };

    getToken()
      .then(async ({ token, livekitUrl }) => {
        if (isStale()) return;
        const livekitUrlForClient = normalizeLivekitUrl(
          livekitUrl || injectedLivekitUrl || `ws://${window.location.hostname}:7880`,
          window.location.protocol === 'https:',
        );
        try {
          let timeout: ReturnType<typeof setTimeout> | undefined;
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
            if (timeout) clearTimeout(timeout);
          }
        } catch (err) {
          // A timed-out connect may still complete later; tear the room down
          // so the error state stays truthful.
          void room.disconnect().catch(() => undefined);
          throw err;
        }
        if (isStale()) {
          room.disconnect();
          return;
        }
        setParticipantCount(room.remoteParticipants.size);
        attachExistingTracks(room, managedStreamRef, setMediaStream, setConnectionStatus, setStatusText);
      })
      .catch((err) => {
        if (isStale()) return;
        connectFailedRef.current = true;
        setConnectionStatus('error');
        // The header badge renders statusText — it must not keep claiming
        // "Connecting..." while the connection actually failed.
        setStatusText('Connection failed');
        setErrorMsg(`Connection failed: ${err instanceof Error ? err.message : String(err)}`);
      });
  }, [roomId]);

  useEffect(() => {
    initializeConnection();
    return () => {
      connectGenRef.current += 1;
      if (stallCheckRef.current) {
        clearInterval(stallCheckRef.current);
        stallCheckRef.current = null;
      }
      if (streamEndTimerRef.current) {
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
        roomRef.current.disconnect();
        roomRef.current = null;
      }
    };
  }, [initializeConnection]);

  const decoderStalledRef = useRef(false);
  useEffect(() => {
    decoderStalledRef.current = decoderStalled;
  }, [decoderStalled]);

  useEffect(() => {
    if (connectionStatus !== 'live') {
      if (stallCheckRef.current) {
        clearInterval(stallCheckRef.current);
        stallCheckRef.current = null;
      }
      stallStartRef.current = 0;
      setDecoderStalled(false);
      setStalledCodec(null);
      return;
    }

    stallStartRef.current = 0;

    stallCheckRef.current = setInterval(async () => {
      const room = roomRef.current;
      if (!room) return;

      const receiver = firstVideoReceiver(room);
      if (!receiver) return;

      const stats = await readVideoStats(receiver);
      evaluateStall(stats, stallStartRef, decoderStalledRef, setDecoderStalled, setStalledCodec);
    }, DECODER_STALL_CHECK_MS);

    return () => {
      if (stallCheckRef.current) {
        clearInterval(stallCheckRef.current);
        stallCheckRef.current = null;
      }
    };
  }, [connectionStatus]);

  const handleResync = () => initializeConnection();

  // Feeds the spectator telemetry bar (VideoPlayer polls this at 2 s).
  const getStatsFn = useCallback(async (): Promise<RTCStatsReport | null> => {
    const room = roomRef.current;
    if (!room) return null;
    const receiver = firstVideoReceiver(room);
    if (!receiver) return null;
    try {
      return await receiver.getStats();
    } catch (err) {
      console.warn('[Room] getStats failed:', err);
      return null;
    }
  }, []);

  useEffect(() => {
    if (!diagnosticEnabled()) return;
    window.__slopcastReceiverStats = getStatsFn;
    return () => {
      delete window.__slopcastReceiverStats;
    };
  }, [getStatsFn]);

  const statusVariant = (): StatusVariant => {
    if (connectionStatus === 'live') return 'live';
    if (connectionStatus === 'connecting') return 'info';
    return 'disconnected';
  };
  const variant = statusVariant();
  const CopyIcon = copied ? Check : Copy;

  const headerFadeClass = isFullscreen && !showFullscreenControls ? 'opacity-0 pointer-events-none' : 'opacity-100';

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
          decoderStalled={decoderStalled}
          stalledCodec={stalledCodec}
          isFullscreen={isFullscreen}
          showFullscreenControls={showFullscreenControls}
        />
      </div>

      <div
        className={`fixed top-0 inset-x-0 bg-gradient-to-b from-black/60 to-transparent px-4 pt-3 pb-8 z-30 pointer-events-none transition-opacity duration-300 ${headerFadeClass}`}
      >
        <div className="flex items-center justify-between pointer-events-auto gap-3">
          <div className="flex items-center gap-2 min-w-0">
            <button
              type="button"
              onClick={() => navigate('/')}
              aria-label="Leave room"
              title="Leave room"
              className="p-2 text-muted-foreground hover:text-foreground hover:bg-white/10 rounded-lg transition-colors duration-200 shrink-0 cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-black"
            >
              <ArrowLeft className="w-5 h-5" />
            </button>
            <span
              role="status"
              aria-live="polite"
              className="inline-flex items-center min-w-0 max-w-[60vw] sm:max-w-[320px] shrink"
            >
              <StatusSignal variant={variant}>{statusText}</StatusSignal>
            </span>
            {participantCount > 0 && (
              <span className="hidden sm:inline-flex items-center gap-2 text-xs font-medium text-white/60 whitespace-nowrap shrink-0 leading-none">
                <span className="size-1.5 rounded-full bg-white/25" aria-hidden="true" />
                <span className="tabular-nums">
                  {participantCount} spectator{participantCount !== 1 ? 's' : ''}
                </span>
              </span>
            )}
          </div>
          <button
            type="button"
            onClick={copyLink}
            aria-label={copied ? 'Link copied' : 'Copy room link'}
            title="Copy room link"
            className="p-2 text-muted-foreground hover:text-foreground hover:bg-white/10 rounded-lg transition-colors duration-200 shrink-0 cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-black"
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

      {errorMsg && (
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
