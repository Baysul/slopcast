import { ConnectionState, type RemoteTrack, Room, RoomEvent } from 'livekit-client';
import { AlertCircle, ArrowLeft, Check, Copy, RefreshCw } from 'lucide-react';
import type React from 'react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { useNavigate, useParams } from 'react-router-dom';
import { SpectatorBanner } from '../components/SpectatorBanner';
import { Badge } from '../components/ui/badge';
import { Button } from '../components/ui/button';
import { VideoPlayer } from '../components/VideoPlayer';

type StatusVariant = 'live' | 'disconnected' | 'info';

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
  const [decoderStalled, setDecoderStalled] = useState(false);
  const [stalledCodec, setStalledCodec] = useState<string | null>(null);

  const roomRef = useRef<Room | null>(null);
  const connectGenRef = useRef(0);
  const managedStreamRef = useRef<MediaStream | null>(null);
  const stallCheckRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const stallStartRef = useRef<number>(0);

  const DECODER_STALL_THRESHOLD_MS = 8000;
  const DECODER_STALL_CHECK_MS = 2000;

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

    if (stallCheckRef.current) {
      clearInterval(stallCheckRef.current);
      stallCheckRef.current = null;
    }
    stallStartRef.current = 0;

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

    const room = new Room({ adaptiveStream: true });
    roomRef.current = room;

    room.on(RoomEvent.TrackSubscribed, (track: RemoteTrack) => {
      if (isStale()) return;
      if (!managedStreamRef.current) {
        managedStreamRef.current = new MediaStream();
      }
      managedStreamRef.current.addTrack(track.mediaStreamTrack);
      setMediaStream(new MediaStream(managedStreamRef.current.getTracks()));
      setConnectionStatus('live');
      setStatusText('Live');

      try {
        const sub = (
          room as { engine?: { pcManager?: { subscriber?: { getRemoteDescription(): RTCSessionDescription | null } } } }
        ).engine?.pcManager?.subscriber;
        if (sub) {
          const desc = sub.getRemoteDescription();
          if (desc) {
            const h264Lines = desc.sdp
              .split('\n')
              .filter((line) => line.startsWith('a=fmtp:') && line.includes('profile-level-id'));
            for (const line of h264Lines) {
              console.log(`[SDP:recv] H264 remote fmtp: ${line}`);
            }
          }
        }
      } catch {
        /* diagnostic-only */
      }
    });

    room.on(RoomEvent.TrackUnsubscribed, (track: RemoteTrack) => {
      if (isStale()) return;
      if (managedStreamRef.current) {
        managedStreamRef.current.removeTrack(track.mediaStreamTrack);
      }
      const hasTracks = [...room.remoteParticipants.values()].some((p) => p.trackPublications.size > 0);
      if (!hasTracks) {
        if (managedStreamRef.current) {
          managedStreamRef.current.getTracks().forEach((t) => {
            t.stop();
          });
          managedStreamRef.current = null;
        }
        setMediaStream(null);
        setConnectionStatus('disconnected');
        setStatusText('Stream ended — waiting for presenter...');
      } else {
        setMediaStream(new MediaStream(managedStreamRef.current?.getTracks() ?? []));
      }
    });

    room.on(RoomEvent.ConnectionStateChanged, (state: ConnectionState) => {
      if (isStale()) return;
      switch (state) {
        case ConnectionState.Reconnecting:
          setStatusText('Reconnecting...');
          break;
        case ConnectionState.Disconnected:
          setConnectionStatus('disconnected');
          setStatusText('Connection lost');
          break;
        default:
          break;
      }
    });

    room.on(RoomEvent.Disconnected, () => {
      if (isStale()) return;
      setConnectionStatus('closed');
      setStatusText('Room closed');
    });

    room.on(RoomEvent.ParticipantConnected, () => {
      if (isStale()) return;
      setParticipantCount(room.remoteParticipants.size);
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
      const res = await fetch(`${baseUrl}/api/rooms/${roomId}/token`);
      if (!res.ok) throw new Error('Failed to fetch spectator token');
      const data = (await res.json()) as { token: string; livekitUrl: string };
      return data;
    };

    getToken()
      .then(({ token, livekitUrl }) => {
        const livekitUrlForClient = livekitUrl || injectedLivekitUrl || `ws://${window.location.hostname}:7880`;
        return room.connect(livekitUrlForClient, token);
      })
      .catch((err) => {
        if (isStale()) return;
        setConnectionStatus('error');
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

      const videoPub = room.remoteParticipants.values().next().value?.videoTrackPublications?.values().next().value;
      const videoTrack = videoPub?.track;
      if (!videoTrack) return;

      const receiver = (videoTrack as { receiver?: RTCRtpReceiver }).receiver;
      if (!receiver) return;

      const stats = await receiver.getStats();
      let packetsReceived = 0;
      let framesDecoded = 0;
      let codecMime: string | null = null;
      let decoderImpl: string | null = null;

      for (const reportRaw of stats.values()) {
        const report = reportRaw as {
          type: string;
          kind?: string;
          packetsReceived?: number;
          framesDecoded?: number;
          mimeType?: string;
          implementation?: string;
        };
        if (report.type === 'inbound-rtp' && report.kind === 'video') {
          packetsReceived = report.packetsReceived ?? 0;
          framesDecoded = report.framesDecoded ?? 0;
        }
        if (report.type === 'codec' && report.mimeType?.toUpperCase()?.includes('VIDEO')) {
          codecMime = report.mimeType ?? null;
          decoderImpl = report.implementation ?? null;
        }
      }

      if (packetsReceived > 0 && framesDecoded === 0) {
        if (stallStartRef.current === 0) {
          stallStartRef.current = Date.now();
          console.warn(
            `[Room] Decoder stall suspected: packets=${packetsReceived} framesDecoded=${framesDecoded} codec=${codecMime} impl=${decoderImpl}`,
          );
        }

        if (Date.now() - stallStartRef.current >= DECODER_STALL_THRESHOLD_MS) {
          setDecoderStalled(true);
          if (codecMime) {
            setStalledCodec(codecMime.replace(/^video\//i, '').toUpperCase());
          }
          console.error(
            `[Room] Decoder stall confirmed after ${DECODER_STALL_THRESHOLD_MS}ms: ` +
              `packets=${packetsReceived} framesDecoded=${framesDecoded} codec=${codecMime} impl=${decoderImpl}`,
          );
        }
      } else if (framesDecoded > 0) {
        stallStartRef.current = 0;
        if (decoderStalled) {
          setDecoderStalled(false);
          setStalledCodec(null);
        }
      }
    }, DECODER_STALL_CHECK_MS);

    return () => {
      if (stallCheckRef.current) {
        clearInterval(stallCheckRef.current);
        stallCheckRef.current = null;
      }
    };
  }, [connectionStatus, decoderStalled]);

  const handleResync = () => initializeConnection();

  const statusVariant = (): StatusVariant => {
    if (connectionStatus === 'live') return 'live';
    if (connectionStatus === 'connecting') return 'info';
    return 'disconnected';
  };
  const variant = statusVariant();

  return (
    <div className="min-h-screen bg-black text-gray-100 relative">
      <div className="absolute inset-0 z-10">
        <VideoPlayer
          mediaStream={mediaStream}
          isLive={connectionStatus === 'live'}
          statusText={statusText}
          onResync={handleResync}
          fullBleed
          decoderStalled={decoderStalled}
          stalledCodec={stalledCodec}
        />
      </div>

      <div className="fixed top-0 inset-x-0 bg-gradient-to-b from-black/60 to-transparent px-4 pt-3 pb-8 z-30 pointer-events-none">
        <div className="flex items-center justify-between pointer-events-auto gap-3">
          <div className="flex items-center gap-2 min-w-0">
            <button
              type="button"
              onClick={() => navigate('/')}
              aria-label="Leave room"
              title="Leave room"
              className="p-2 text-gray-400 hover:text-gray-100 hover:bg-white/10 rounded-lg transition-colors duration-200 shrink-0 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-black"
            >
              <ArrowLeft className="w-5 h-5" />
            </button>
            <span role="status" aria-live="polite" className="min-w-0">
              <Badge
                variant={variant}
                className="min-w-0 max-w-[60vw] sm:max-w-[320px] shrink transition-colors duration-300"
              >
                {connectionStatus === 'live' && (
                  <span className="relative w-1.5 h-1.5 shrink-0" aria-hidden="true">
                    <span className="absolute inset-0 rounded-full bg-safelight animate-ping opacity-75" />
                    <span className="absolute inset-0 rounded-full bg-safelight" />
                  </span>
                )}
                <span className="truncate min-w-0">{statusText}</span>
              </Badge>
            </span>
            {participantCount > 0 && (
              <span className="hidden sm:inline-flex items-center rounded-full px-2.5 py-0.5 text-xs font-medium text-gray-400 bg-black/40 border border-white/10 backdrop-blur-md shrink-0">
                {participantCount} spectator{participantCount !== 1 ? 's' : ''}
              </span>
            )}
          </div>
          <button
            type="button"
            onClick={copyLink}
            aria-label={copied ? 'Link copied' : 'Copy room link'}
            title="Copy room link"
            className="p-2 text-gray-400 hover:text-gray-100 hover:bg-white/10 rounded-lg transition-colors duration-200 shrink-0 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-safelight/70 focus-visible:ring-offset-2 focus-visible:ring-offset-black"
          >
            {copied ? <Check className="w-4 h-4" /> : <Copy className="w-4 h-4" />}
          </button>
        </div>
      </div>

      <div className="fixed bottom-4 left-4 z-30 pointer-events-none">
        <SpectatorBanner compact />
      </div>

      {errorMsg && (
        <div className="fixed bottom-6 inset-x-0 flex justify-center px-4 z-30 pointer-events-none">
          <div
            role="alert"
            className="bg-black/80 border border-destructive/25 text-destructive px-4 py-3 rounded-xl flex items-center gap-3 flex-wrap max-w-[90vw] sm:max-w-md backdrop-blur-md pointer-events-auto shadow-lg"
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
