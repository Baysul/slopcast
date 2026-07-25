import { ConnectionState, Room, RoomEvent } from 'livekit-client';
import { AlertCircle, ArrowLeft, Check, Copy, RefreshCw } from 'lucide-react';
import type React from 'react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { useNavigate, useParams } from 'react-router-dom';
import { Badge } from '../components/ui/Badge';
import { Button } from '../components/ui/Button';
import { VideoPlayer } from '../components/VideoPlayer';

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

  const roomRef = useRef<Room | null>(null);
  const connectGenRef = useRef(0);

  const copyLink = () => {
    navigator.clipboard.writeText(window.location.href).catch(() => {});
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

    if (roomRef.current) {
      roomRef.current.removeAllListeners();
      roomRef.current.disconnect();
      roomRef.current = null;
    }

    const room = new Room({ adaptiveStream: true });
    roomRef.current = room;

    room.on(RoomEvent.TrackSubscribed, (track) => {
      if (isStale()) return;
      const stream = track.mediaStream;
      if (stream) {
        setMediaStream(stream);
      }
      setConnectionStatus('live');
      setStatusText('Live');
    });

    room.on(RoomEvent.TrackUnsubscribed, () => {
      if (isStale()) return;
      const hasTracks = [...room.remoteParticipants.values()].some((p) => p.trackPublications.size > 0);
      if (!hasTracks) {
        setMediaStream(null);
        setConnectionStatus('disconnected');
        setStatusText('Stream ended — waiting for presenter...');
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

    const livekitUrl = (window as { __SLOPCAST_CONFIG__?: { livekitUrl?: string } }).__SLOPCAST_CONFIG__?.livekitUrl;

    const apiEndpoint = (window as { __SLOPCAST_CONFIG__?: { apiEndpoint?: string } }).__SLOPCAST_CONFIG__?.apiEndpoint;
    const baseUrl = livekitUrl
      ? livekitUrl.replace(/^ws(s?):\/\//, 'http$1://')
      : apiEndpoint
        ? apiEndpoint
        : `${window.location.protocol}//${window.location.hostname}:3001`;

    const getToken = async (): Promise<string> => {
      const res = await fetch(`${baseUrl}/api/rooms/${roomId}/token`);
      if (!res.ok) throw new Error('Failed to fetch spectator token');
      const data = (await res.json()) as { token: string };
      return data.token;
    };

    const livekitUrlForClient = livekitUrl || `ws://${window.location.hostname}:7880`;

    getToken()
      .then((token) => room.connect(livekitUrlForClient, token))
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
      if (roomRef.current) {
        roomRef.current.removeAllListeners();
        roomRef.current.disconnect();
        roomRef.current = null;
      }
    };
  }, [initializeConnection]);

  const handleResync = () => initializeConnection();

  const statusVariant: 'live' | 'disconnected' | 'info' =
    connectionStatus === 'live'
      ? 'live'
      : connectionStatus === 'disconnected' || connectionStatus === 'closed' || connectionStatus === 'error'
        ? 'disconnected'
        : 'info';

  const statusOverride =
    statusVariant === 'live'
      ? 'bg-safelight/15 border-safelight/25'
      : statusVariant === 'disconnected'
        ? 'bg-destructive/15 border-destructive/25'
        : 'bg-white/5 text-gray-400 border-white/10';

  return (
    <div className="min-h-screen bg-black text-gray-100 relative">
      <div className="absolute inset-0 z-10">
        <VideoPlayer
          mediaStream={mediaStream}
          isLive={connectionStatus === 'live'}
          statusText={statusText}
          onResync={handleResync}
          fullBleed
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
                variant={statusVariant}
                className={`min-w-0 max-w-[60vw] sm:max-w-[320px] shrink transition-colors duration-300 ${statusOverride}`}
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
