import React, { useEffect, useState, useRef, useCallback } from 'react';
import { useParams, useNavigate } from 'react-router-dom';
import type { Participant, JoinedRoomPayload } from '@screen-share/shared-types';
import { SignalingClient } from '../services/SignalingClient';
import { WebRTCReceiver, WebRTCConnectionState } from '../services/WebRTCReceiver';
import { Header } from '../components/Header';
import { SpectatorBanner } from '../components/SpectatorBanner';
import { VideoPlayer } from '../components/VideoPlayer';
import { ParticipantList } from '../components/ParticipantList';
import { Button } from '../components/ui/Button';
import { Card } from '../components/ui/Card';
import { ShieldAlert, RefreshCw, Home, Users, AlertCircle } from 'lucide-react';

export const RoomPage: React.FC = () => {
  const { roomId } = useParams<{ roomId: string }>();
  const navigate = useNavigate();

  const [connectionStatus, setConnectionStatus] = useState<
    'connecting' | 'live' | 'disconnected' | 'closed' | 'error'
  >('connecting');
  const [statusText, setStatusText] = useState('Connecting to signaling server...');
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const [participants, setParticipants] = useState<Participant[]>([]);
  const [myUserId, setMyUserId] = useState<string>('');
  const [mediaStream, setMediaStream] = useState<MediaStream | null>(null);
  const [rtcState, setRtcState] = useState<WebRTCConnectionState>('new');

  const signalingRef = useRef<SignalingClient | null>(null);
  const rtcReceiverRef = useRef<WebRTCReceiver | null>(null);
  const connectGenRef = useRef(0);

  const initializeConnection = useCallback(() => {
    if (!roomId) return;

    const gen = ++connectGenRef.current;
    const isStale = () => connectGenRef.current !== gen;

    // Reset state
    setConnectionStatus('connecting');
    setStatusText('Connecting to signaling server...');
    setErrorMsg(null);
    setMediaStream(null);
    setParticipants([]);

    // Clean up old references
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

    // Setup WebRTC Receiver
    const receiver = new WebRTCReceiver(client, {
      onStream: (stream) => {
        if (isStale()) return;
        console.log('[RoomPage] Received MediaStream');
        setMediaStream(stream);
        setConnectionStatus('live');
        setStatusText('Live Stream Active');
      },
      onStateChange: (state) => {
        if (isStale()) return;
        console.log('[RoomPage] WebRTC State:', state);
        setRtcState(state);
        if (state === 'connected') {
          setConnectionStatus('live');
          setStatusText('Live Stream Active');
        } else if (state === 'connecting') {
          setStatusText('Connecting to presenter stream...');
        } else if (state === 'disconnected' || state === 'failed') {
          setStatusText('Stream connection dropped');
        }
      },
      onError: (err) => {
        if (isStale()) return;
        console.error('[RoomPage] WebRTC Receiver Error:', err);
        setErrorMsg('WebRTC Connection Error: ' + err.message);
      },
      onPublishNotice: () => {
        if (isStale()) return;
        setStatusText('Presenter started streaming — connecting...');
      },
    });
    rtcReceiverRef.current = receiver;

    // Signaling Event Listeners
    client.on('connected', () => {
      if (isStale()) return;
      setErrorMsg(null);
      setStatusText(`Joining room ${roomId}...`);
      client.joinRoom(roomId);
    });

    client.on('joined_room', (payload: JoinedRoomPayload) => {
      if (isStale()) return;
      console.log('[RoomPage] Joined room:', payload);
      setConnectionStatus('connecting');
      setErrorMsg(null);
      setParticipants(payload.participants);
      if (payload.assignedId) {
        setMyUserId(payload.assignedId);
      }

      if (payload.isStreaming) {
        setStatusText('Presenter is live — connecting to stream...');
      } else {
        const hasPresenter = payload.participants.some((p) => p.role === 'presenter');
        setStatusText(
          hasPresenter
            ? 'Waiting for presenter stream...'
            : 'Room connected. Waiting for presenter to start stream...'
        );
      }
    });

    client.on('role_assignment', (payload) => {
      if (payload.reason) {
        console.log('[RoomPage] Role notice:', payload.reason);
      }
    });

    client.on('user_joined', (participant) => {
      if (isStale()) return;
      console.log('[RoomPage] User joined:', participant);
      setParticipants((prev) => {
        if (prev.some((p) => p.id === participant.id)) return prev;
        return [...prev, participant];
      });
    });

    client.on('user_left', (userId) => {
      if (isStale()) return;
      console.log('[RoomPage] User left:', userId);
      setParticipants((prev) => prev.filter((p) => p.id !== userId));
    });

    client.on('room_closed', (payload) => {
      if (isStale()) return;
      console.log('[RoomPage] Room closed:', payload.reason);
      setConnectionStatus('closed');
      setStatusText('Room Closed');
      setErrorMsg(payload.reason || 'The presenter closed the session.');
      if (rtcReceiverRef.current) {
        rtcReceiverRef.current.close();
      }
    });

    client.on('error', (payload) => {
      if (isStale()) return;
      console.error('[RoomPage] Signaling Error:', payload.message);
      setConnectionStatus('error');
      setErrorMsg(payload.message);
    });

    client.on('disconnected', (reason) => {
      if (isStale()) return;
      console.warn('[RoomPage] Signaling Disconnected:', reason);
      setConnectionStatus('disconnected');
      setStatusText('Signaling connection lost');
    });

    client.connect().catch((err) => {
      if (isStale()) return;
      console.error('[RoomPage] Failed to connect to signaling server:', err);
      setConnectionStatus('error');
      setErrorMsg(`Could not connect to signaling server at ${client.getUrl()}`);
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

  const handleResync = () => {
    initializeConnection();
  };

  return (
    <div className="min-h-screen flex flex-col bg-[#090d16] text-gray-100">
      <Header
        roomCode={roomId}
        status={connectionStatus}
        statusText={statusText}
      />

      <SpectatorBanner />

      <main className="flex-1 max-w-7xl mx-auto w-full p-4 sm:p-6 grid grid-cols-1 lg:grid-cols-4 gap-6">
        {/* Main Video View Column */}
        <div className="lg:col-span-3 space-y-4">
          {errorMsg && (
            <div className="bg-rose-950/60 border border-rose-500/30 text-rose-200 p-4 rounded-xl flex items-center justify-between gap-4 backdrop-blur-md">
              <div className="flex items-center gap-3">
                <AlertCircle className="w-5 h-5 text-rose-400 shrink-0" />
                <span className="text-sm font-medium">{errorMsg}</span>
              </div>
              <Button size="sm" variant="outline" onClick={handleResync} className="shrink-0 border-rose-500/30 text-rose-200 hover:bg-rose-900/50">
                <RefreshCw className="w-3.5 h-3.5" />
                <span>Retry</span>
              </Button>
            </div>
          )}

          <VideoPlayer
            mediaStream={mediaStream}
            isLive={connectionStatus === 'live'}
            statusText={statusText}
            onResync={handleResync}
          />
        </div>

        {/* Sidebar Column */}
        <div className="space-y-4">
          <ParticipantList
            participants={participants}
            currentUserId={myUserId}
          />

          {/* Quick Info Card */}
          <Card className="p-4 space-y-3">
            <div className="flex items-center gap-2 font-medium text-gray-200 text-xs uppercase tracking-wider">
              <ShieldAlert className="w-4 h-4 text-indigo-400" />
              <span>Spectator Mode Rules</span>
            </div>
            <p className="text-xs text-gray-400 leading-relaxed">
              This browser window is connected as a spectator. Screensharing is disabled in web browsers to preserve system resources and enforce PipeWire/WASAPI per-app audio filtering via the native Desktop app.
            </p>
            <div className="pt-2 border-t border-gray-800 flex items-center justify-between">
              <Button size="sm" variant="ghost" onClick={() => navigate('/')} className="w-full justify-center text-xs gap-2">
                <Home className="w-3.5 h-3.5" />
                <span>Return to Home</span>
              </Button>
            </div>
          </Card>
        </div>
      </main>
    </div>
  );
};
