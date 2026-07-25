import type { JoinedRoomPayload, Participant } from '@screen-share/shared-types';
import { AlertCircle, ArrowLeft, Check, Copy, RefreshCw } from 'lucide-react';
import type React from 'react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { useNavigate, useParams } from 'react-router-dom';
import { Badge } from '../components/ui/Badge';
import { Button } from '../components/ui/Button';
import { VideoPlayer } from '../components/VideoPlayer';
import { SignalingClient } from '../services/SignalingClient';
import { type WebRTCConnectionState, WebRTCReceiver } from '../services/WebRTCReceiver';

export const RoomPage: React.FC = () => {
  const { roomId } = useParams<{ roomId: string }>();
  const navigate = useNavigate();

  const [connectionStatus, setConnectionStatus] = useState<'connecting' | 'live' | 'disconnected' | 'closed' | 'error'>(
    'connecting',
  );
  const [statusText, setStatusText] = useState('Connecting...');
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const [participants, setParticipants] = useState<Participant[]>([]);
  const [_myUserId, setMyUserId] = useState<string>('');
  const [mediaStream, setMediaStream] = useState<MediaStream | null>(null);
  const [copied, setCopied] = useState(false);
  const [_rtcState, setRtcState] = useState<WebRTCConnectionState>('new');

  const signalingRef = useRef<SignalingClient | null>(null);
  const rtcReceiverRef = useRef<WebRTCReceiver | null>(null);
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
    setParticipants([]);

    if (rtcReceiverRef.current) {
      rtcReceiverRef.current.close();
      rtcReceiverRef.current = null;
    }
    if (signalingRef.current) {
      signalingRef.current.disconnect();
      signalingRef.current = null;
    }

    const client = new SignalingClient();
    signalingRef.current = client;

    const receiver = new WebRTCReceiver(client, {
      onStream: (stream) => {
        if (isStale()) return;
        setMediaStream(stream);
        setConnectionStatus('live');
        setStatusText('Live');
      },
      onStateChange: (state) => {
        if (isStale()) return;
        setRtcState(state);
        if (state === 'connected') {
          setConnectionStatus('live');
          setStatusText('Live');
        } else if (state === 'connecting') {
          setStatusText('Connecting...');
        } else if (state === 'disconnected' || state === 'failed') {
          setStatusText('Disconnected');
        }
      },
      onError: (err) => {
        if (isStale()) return;
        setErrorMsg(`WebRTC error: ${err.message}`);
      },
      onPublishNotice: () => {
        if (isStale()) return;
        setStatusText('Presenter streaming — connecting...');
      },
      onStreamEnd: () => {
        if (isStale()) return;
        setMediaStream(null);
        setConnectionStatus('connecting');
        setStatusText('Stream ended — waiting for presenter...');
      },
    });
    rtcReceiverRef.current = receiver;

    client.on('connected', () => {
      if (isStale()) return;
      setErrorMsg(null);
      setStatusText('Joining room...');
      client.joinRoom(roomId);
    });

    client.on('joined_room', (payload: JoinedRoomPayload) => {
      if (isStale()) return;
      setConnectionStatus('connecting');
      setErrorMsg(null);
      setParticipants(payload.participants);
      if (payload.assignedId) setMyUserId(payload.assignedId);

      if (payload.isStreaming) {
        setStatusText('Presenter is live — connecting...');
      } else {
        const hasPresenter = payload.participants.some((p) => p.role === 'presenter');
        setStatusText(hasPresenter ? 'Waiting for stream...' : 'Room connected. Waiting for presenter...');
      }
    });

    client.on('role_assignment', (payload) => {
      if (payload.reason) console.log('[RoomPage] Role notice:', payload.reason);
    });

    client.on('user_joined', (participant) => {
      if (isStale()) return;
      setParticipants((prev) => {
        if (prev.some((p) => p.id === participant.id)) return prev;
        return [...prev, participant];
      });
    });

    client.on('user_left', (userId) => {
      if (isStale()) return;
      setParticipants((prev) => prev.filter((p) => p.id !== userId));
    });

    client.on('room_closed', (payload) => {
      if (isStale()) return;
      setConnectionStatus('closed');
      setStatusText('Room Closed');
      setErrorMsg(payload.reason || 'The presenter closed the session.');
      if (rtcReceiverRef.current) rtcReceiverRef.current.close();
    });

    client.on('stop_stream', () => {
      if (isStale()) return;
      setMediaStream(null);
      setConnectionStatus('connecting');
      setStatusText('Stream ended — waiting for presenter...');
      if (rtcReceiverRef.current) rtcReceiverRef.current.close();
    });

    client.on('error', (payload) => {
      if (isStale()) return;
      setConnectionStatus('error');
      setErrorMsg(payload.message);
    });

    client.on('disconnected', (reason) => {
      if (isStale()) return;
      console.warn('[RoomPage] Signaling Disconnected:', reason);
      setConnectionStatus('disconnected');
      setStatusText('Connection lost');
    });

    client.connect().catch((_err) => {
      if (isStale()) return;
      setConnectionStatus('error');
      setErrorMsg('Could not connect to signaling server');
    });
  }, [roomId]);

  useEffect(() => {
    initializeConnection();
    return () => {
      connectGenRef.current += 1;
      if (rtcReceiverRef.current) {
        rtcReceiverRef.current.close();
        rtcReceiverRef.current = null;
      }
      if (signalingRef.current) {
        signalingRef.current.disconnect();
        signalingRef.current = null;
      }
    };
  }, [initializeConnection]);

  const handleResync = () => initializeConnection();

  const getStatsFn = useCallback(async () => rtcReceiverRef.current?.getStats() ?? null, []);

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
          getStatsFn={getStatsFn}
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
            {participants.length > 0 && (
              <span className="hidden sm:inline-flex items-center rounded-full px-2.5 py-0.5 text-xs font-medium text-gray-400 bg-black/40 border border-white/10 backdrop-blur-md shrink-0">
                {participants.length} spectator{participants.length !== 1 ? 's' : ''}
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
