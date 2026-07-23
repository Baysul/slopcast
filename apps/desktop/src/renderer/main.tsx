import React, { useEffect, useState, useRef } from 'react';
import ReactDOM from 'react-dom/client';
import './index.css';

declare global {
  interface Window {
    electronAPI?: {
      getPlatformInfo: () => Promise<{ platform: string; isWayland: boolean }>;
      getAudioApps: () => Promise<Array<{ id: number; name: string; processId: number }>>;
      startAudioCapture: (targetId: number) => Promise<boolean>;
      stopAudioCapture: () => Promise<boolean>;
      getDesktopSources: () => Promise<Array<{ id: string; name: string; thumbnail: string }>>;
      clipboardWriteText: (text: string) => Promise<boolean>;
      resolveAudioSource: (opts: {
        sourceId?: string;
        trackLabel?: string;
      }) => Promise<{ id: number; name: string; processId: number } | null>;
    };
  }
}

async function copyText(text: string): Promise<boolean> {
  if (!text) return false;
  try {
    if (window.electronAPI?.clipboardWriteText) {
      return await window.electronAPI.clipboardWriteText(text);
    }
    await navigator.clipboard.writeText(text);
    return true;
  } catch (err) {
    console.error('copyText failed:', err);
    return false;
  }
}

interface AudioApp {
  id: number;
  name: string;
  processId: number;
}

interface DesktopSource {
  id: string;
  name: string;
  thumbnail: string;
}

/**
 * Finds the native virtual capture microphone ("Screenshare Window Audio").
 * Chromium filters PipeWire sink-monitor sources out of getUserMedia, so the
 * native layer exposes an Audio/Source/Virtual node instead. Device labels are
 * hidden until microphone access has been granted once, so this unlocks labels
 * on demand.
 */
const findCaptureAudioDevice = async (): Promise<MediaDeviceInfo | null> => {
  let devices = await navigator.mediaDevices.enumerateDevices();
  if (devices.some((d) => d.kind === 'audioinput' && !d.label)) {
    const unlock = await navigator.mediaDevices.getUserMedia({ audio: true });
    unlock.getTracks().forEach((t) => t.stop());
    devices = await navigator.mediaDevices.enumerateDevices();
  }
  return (
    devices.find(
      (d) =>
        d.kind === 'audioinput' &&
        (d.label.toLowerCase().includes('screenshare') ||
          d.label.toLowerCase().includes('screenshare-window-audio'))
    ) ?? null
  );
};

/**
 * Fuzzy-matches an audio app name against a query string (e.g. a window
 * title or track label). Returns the best match or null.
 */
const findBestAudioMatch = (apps: AudioApp[], query: string): AudioApp | null => {
  const q = query.toLowerCase();

  let best = apps.find((a) => a.name.toLowerCase() === q);
  if (best) return best;

  best = apps.find((a) => q.includes(a.name.toLowerCase()));
  if (best) return best;

  best = apps.find((a) => a.name.toLowerCase().includes(q));
  if (best) return best;

  const firstWord = q.split(/\s+/)[0];
  if (firstWord) {
    best = apps.find((a) => a.name.toLowerCase().includes(firstWord) || firstWord.includes(a.name.toLowerCase()));
    if (best) return best;
  }

  return null;
};

export const PresenterApp: React.FC = () => {
  const [roomCode, setRoomCode] = useState<string>('');
  const [shareUrl, setShareUrl] = useState<string>('');
  const [isWayland, setIsWayland] = useState<boolean>(false);
  const [audioApps, setAudioApps] = useState<AudioApp[]>([]);
  const [selectedAudioAppId, setSelectedAudioAppId] = useState<number | null>(null);
  const [autoDetectedApp, setAutoDetectedApp] = useState<AudioApp | null>(null);
  const [desktopSources, setDesktopSources] = useState<DesktopSource[]>([]);
  const [selectedSourceId, setSelectedSourceId] = useState<string>('');
  const [isSharing, setIsSharing] = useState<boolean>(false);
  const [copied, setCopied] = useState<'link' | 'code' | null>(null);
  const [statusMsg, setStatusMsg] = useState<string>('Ready to create room');
  const [previewStream, setPreviewStream] = useState<MediaStream | null>(null);
  const [spectatorCount, setSpectatorCount] = useState(0);

  const wsRef = useRef<WebSocket | null>(null);
  const peerConnectionsRef = useRef<Map<string, RTCPeerConnection>>(new Map());
  const localStreamRef = useRef<MediaStream | null>(null);
  const isSharingRef = useRef(false);
  const roomCodeRef = useRef('');
  const spectatorIdsRef = useRef<Set<string>>(new Set());
  const pendingCandidatesRef = useRef<Map<string, RTCIceCandidateInit[]>>(new Map());
  const handleSignalingMessageRef = useRef<(msg: any) => Promise<void>>(async () => {});
  const previewVideoRef = useRef<HTMLVideoElement | null>(null);

  useEffect(() => {
    isSharingRef.current = isSharing;
  }, [isSharing]);

  useEffect(() => {
    roomCodeRef.current = roomCode;
  }, [roomCode]);

  // Bind the live capture stream to the local preview <video>.
  useEffect(() => {
    const el = previewVideoRef.current;
    if (!el) return;
    el.srcObject = previewStream;
    if (previewStream) {
      el.play().catch(() => {
        /* autoplay can fail until user gesture; muted should make it ok */
      });
    }
  }, [previewStream]);

  useEffect(() => {
    (async () => {
      if (window.electronAPI) {
        const info = await window.electronAPI.getPlatformInfo();
        setIsWayland(info.isWayland);
        if (!info.isWayland) {
          loadDesktopSources();
        }
      }
      loadAudioApps();
    })();

    return () => {
      peerConnectionsRef.current.forEach((pc) => pc.close());
      peerConnectionsRef.current.clear();
      if (wsRef.current) {
        wsRef.current.close();
        wsRef.current = null;
      }
    };
  }, []);

  const loadAudioApps = async () => {
    if (window.electronAPI) {
      const apps = await window.electronAPI.getAudioApps();
      setAudioApps(apps);
    }
  };

  const loadDesktopSources = async () => {
    if (window.electronAPI) {
      const sources = await window.electronAPI.getDesktopSources();
      setDesktopSources(sources);
    }
  };

  const attemptAutoResolve = async (opts: {
    sourceId?: string;
    trackLabel?: string;
  }): Promise<AudioApp | null> => {
    if (!window.electronAPI) return null;

    let app = await window.electronAPI.resolveAudioSource(opts);

    if (!app && opts.trackLabel && audioApps.length > 0) {
      app = findBestAudioMatch(audioApps, opts.trackLabel);
    }

    if (app) {
      setAutoDetectedApp(app);
      setSelectedAudioAppId(app.id);
      setStatusMsg(`Auto-detected audio source: ${app.name}`);
      return app;
    }
    return null;
  };

  const createOfferForSpectator = async (spectatorId: string) => {
    if (!localStreamRef.current || !wsRef.current) {
      console.warn(`[Presenter] Cannot offer to ${spectatorId}: no local stream or ws`);
      return;
    }
    if (wsRef.current.readyState !== WebSocket.OPEN) {
      console.warn(`[Presenter] Cannot offer to ${spectatorId}: ws not open`);
      return;
    }

    // Replace any existing PC for this spectator (re-offer after renegotiation).
    const existing = peerConnectionsRef.current.get(spectatorId);
    if (existing) {
      existing.close();
      peerConnectionsRef.current.delete(spectatorId);
    }

    const pc = new RTCPeerConnection({
      iceServers: [
        { urls: 'stun:stun.l.google.com:19302' },
        { urls: 'stun:stun1.l.google.com:19302' },
      ],
    });

    peerConnectionsRef.current.set(spectatorId, pc);
    pendingCandidatesRef.current.set(spectatorId, []);

    const stream = localStreamRef.current;
    for (const track of stream.getTracks()) {
      console.log(`[Presenter] addTrack ${track.kind} readyState=${track.readyState} to ${spectatorId}`);
      pc.addTrack(track, stream);
    }

    pc.onicecandidate = (event) => {
      if (event.candidate && wsRef.current?.readyState === WebSocket.OPEN) {
        wsRef.current.send(
          JSON.stringify({
            type: 'WEBRTC_SIGNAL',
            payload: {
              targetId: spectatorId,
              signal: { type: 'candidate', candidate: event.candidate.toJSON() },
            },
          })
        );
      }
    };

    pc.onconnectionstatechange = () => {
      console.log(`[Presenter] PC ${spectatorId} state:`, pc.connectionState);
      if (pc.connectionState === 'failed' || pc.connectionState === 'closed') {
        peerConnectionsRef.current.delete(spectatorId);
      }
    };

    try {
      const offer = await pc.createOffer();
      await pc.setLocalDescription(offer);

      const payload = {
        type: 'WEBRTC_SIGNAL',
        payload: {
          targetId: spectatorId,
          signal: { type: 'offer', sdp: offer.sdp },
        },
      };
      wsRef.current.send(JSON.stringify(payload));
      console.log(`[Presenter] Sent offer to spectator ${spectatorId} (sdp ${offer.sdp?.length ?? 0} bytes)`);
    } catch (err) {
      console.error(`[Presenter] Failed to create offer for ${spectatorId}:`, err);
      pc.close();
      peerConnectionsRef.current.delete(spectatorId);
    }
  };

  /** Merge local tracking with server room state so we never miss a spectator. */
  const resolveSpectatorIds = async (hintIds?: string[]): Promise<string[]> => {
    const ids = new Set<string>(spectatorIdsRef.current);
    if (hintIds) {
      for (const id of hintIds) ids.add(id);
    }

    const code = roomCodeRef.current;
    if (code) {
      try {
        const res = await fetch(`http://localhost:3001/api/rooms/${encodeURIComponent(code)}`);
        if (res.ok) {
          const room = await res.json();
          const participants = room.participants || {};
          for (const p of Object.values(participants) as Array<{ id: string; role: string }>) {
            if (p.role === 'spectator' && p.id) {
              ids.add(p.id);
              spectatorIdsRef.current.add(p.id);
            }
          }
        }
      } catch (err) {
        console.warn('[Presenter] Failed to fetch room participants:', err);
      }
    }

    setSpectatorCount(ids.size);
    return Array.from(ids);
  };

  const offerToAllSpectators = async (hintIds?: string[]) => {
    const ids = await resolveSpectatorIds(hintIds);
    console.log(`[Presenter] Offering stream to ${ids.length} spectator(s):`, ids);
    if (ids.length === 0) {
      setStatusMsg('Streaming live — waiting for spectators to join...');
      return;
    }
    setStatusMsg(`Streaming live — connecting ${ids.length} spectator(s)...`);
    await Promise.all(ids.map((id) => createOfferForSpectator(id)));
  };

  const handleSignalingMessage = async (msg: any) => {
    const { type, payload } = msg;
    console.log('[Presenter] signaling message:', type, payload);

    if (type === 'ROOM_CREATED') {
      const code = payload.code as string;
      const url =
        (payload.shareUrl as string | undefined) || `http://localhost:3000/room/${code}`;
      roomCodeRef.current = code;
      setRoomCode(code);
      setShareUrl(url);
      setStatusMsg(`Room active: ${code}`);
      spectatorIdsRef.current.clear();
      setSpectatorCount(0);

      // Auto-copy the join link as soon as the room exists.
      void copyText(url).then((ok) => {
        if (ok) {
          setCopied('link');
          setStatusMsg(`Room active: ${code} — link copied to clipboard`);
          setTimeout(() => setCopied(null), 2500);
        }
      });
    } else if (type === 'USER_JOINED') {
      const spectatorId = payload.participant?.id as string | undefined;
      if (!spectatorId) return;

      spectatorIdsRef.current.add(spectatorId);
      setSpectatorCount(spectatorIdsRef.current.size);
      setStatusMsg(`Spectator ${spectatorId} joined room`);

      // Offer immediately if already streaming (refs avoid stale React state).
      if (isSharingRef.current && localStreamRef.current) {
        void createOfferForSpectator(spectatorId);
      }
    } else if (type === 'USER_LEFT') {
      const userId = payload.userId as string | undefined;
      if (!userId) return;
      spectatorIdsRef.current.delete(userId);
      setSpectatorCount(spectatorIdsRef.current.size);
      const pc = peerConnectionsRef.current.get(userId);
      if (pc) {
        pc.close();
        peerConnectionsRef.current.delete(userId);
      }
      pendingCandidatesRef.current.delete(userId);
    } else if (type === 'PUBLISH_ACK') {
      // Server-authoritative list of spectators currently in the room.
      const spectatorIds = (payload?.spectatorIds as string[] | undefined) || [];
      console.log('[Presenter] PUBLISH_ACK spectators:', spectatorIds);
      for (const id of spectatorIds) spectatorIdsRef.current.add(id);
      setSpectatorCount(spectatorIdsRef.current.size);
      if (isSharingRef.current && localStreamRef.current) {
        void offerToAllSpectators(spectatorIds);
      }
    } else if (type === 'PUBLISH_REJECTED') {
      console.error('[Presenter] PUBLISH_REJECTED:', payload?.reason);
      setStatusMsg('Publish rejected: ' + (payload?.reason || 'unknown'));
    } else if (type === 'WEBRTC_SIGNAL') {
      const { senderId, signal } = payload;
      if (!senderId || !signal) return;

      const pc = peerConnectionsRef.current.get(senderId);
      if (!pc) {
        // Queue ICE until offer path creates the PC (should be rare).
        if (signal.candidate || signal.type === 'candidate') {
          const list = pendingCandidatesRef.current.get(senderId) || [];
          list.push(signal.candidate || signal);
          pendingCandidatesRef.current.set(senderId, list);
        }
        return;
      }

      try {
        if (signal.type === 'answer') {
          if (pc.signalingState === 'have-local-offer') {
            await pc.setRemoteDescription(new RTCSessionDescription(signal));
            const queued = pendingCandidatesRef.current.get(senderId) || [];
            for (const c of queued) {
              await pc.addIceCandidate(new RTCIceCandidate(c));
            }
            pendingCandidatesRef.current.set(senderId, []);
            console.log(`[Presenter] Applied answer from ${senderId}`);
          } else {
            console.warn(
              `[Presenter] Ignoring answer from ${senderId}; signalingState=${pc.signalingState}`
            );
          }
        } else if (signal.candidate || signal.type === 'candidate') {
          const candidateInit = signal.candidate || signal;
          if (pc.remoteDescription) {
            await pc.addIceCandidate(new RTCIceCandidate(candidateInit));
          } else {
            const list = pendingCandidatesRef.current.get(senderId) || [];
            list.push(candidateInit);
            pendingCandidatesRef.current.set(senderId, list);
          }
        }
      } catch (err) {
        console.error(`[Presenter] Error handling signal from ${senderId}:`, err);
      }
    }
  };

  // Keep WS handler pointed at the latest closure without re-binding the socket.
  handleSignalingMessageRef.current = handleSignalingMessage;

  const handleCreateRoom = () => {
    if (wsRef.current) {
      wsRef.current.close();
    }

    const ws = new WebSocket('ws://localhost:3001');
    wsRef.current = ws;

    ws.onopen = () => {
      setStatusMsg('Connected to signaling server');
      ws.send(
        JSON.stringify({
          type: 'CREATE_ROOM',
          payload: { clientOrigin: 'desktop' },
        })
      );
    };

    ws.onmessage = async (event) => {
      try {
        const msg = JSON.parse(event.data);
        await handleSignalingMessageRef.current(msg);
      } catch (err) {
        console.error('Signaling message parse error:', err);
      }
    };

    ws.onclose = () => {
      setStatusMsg('Disconnected from signaling server');
      setRoomCode('');
      setShareUrl('');
      spectatorIdsRef.current.clear();
      setSpectatorCount(0);
    };

    ws.onerror = (err) => {
      console.error('WebSocket error:', err);
      setStatusMsg('Connection error');
    };
  };

  /**
   * Captures the video track of the window to share. On Wayland the window
   * selection happens in the native xdg-desktop-portal dialog; on X11 the
   * in-app source picker selection is used.
   */
  const captureVideoTrack = async (): Promise<MediaStreamTrack> => {
    if (isWayland) {
      // The main-process displayMediaRequestHandler answers this request;
      // xdg-desktop-portal shows the desktop environment's own window picker.
      const stream = await navigator.mediaDevices.getDisplayMedia({
        video: { frameRate: { ideal: 60, max: 60 } },
        audio: false,
      });
      const track = stream.getVideoTracks()[0];
      if (!track) {
        throw new Error('xdg-desktop-portal granted no video track');
      }
      return track;
    }

    if (!selectedSourceId) {
      throw new Error('No capture source selected');
    }
    const stream = await (navigator.mediaDevices as any).getUserMedia({
      audio: false,
      video: {
        mandatory: {
          chromeMediaSource: 'desktop',
          chromeMediaSourceId: selectedSourceId,
          minFrameRate: 30,
          maxFrameRate: 60,
        },
      },
    });
    return stream.getVideoTracks()[0];
  };

  /**
   * Starts exclusive native capture of the selected application's audio and
   * returns its audio track from the virtual capture microphone. ONLY the
   * selected application's audio is captured.
   */
  const captureAudioTrack = async (targetId: number): Promise<MediaStreamTrack | null> => {
    const started = await window.electronAPI!.startAudioCapture(targetId);
    if (!started) {
      throw new Error('Native audio capture failed to start');
    }

    // The virtual mic can take a moment to appear in Chromium's device list;
    // poll briefly for it.
    for (let attempt = 0; attempt < 40; attempt++) {
      const device = await findCaptureAudioDevice();
      if (device) {
        const stream = await navigator.mediaDevices.getUserMedia({
          audio: {
            deviceId: { exact: device.deviceId },
            echoCancellation: false,
            noiseSuppression: false,
            autoGainControl: false,
          },
        });
        const track = stream.getAudioTracks()[0];
        if (track) {
          return track;
        }
      }
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
    throw new Error('Virtual capture microphone did not appear as an audio device');
  };

  const handleStartShare = async () => {
    try {
      setStatusMsg('Starting capture...');
      const videoTrack = await captureVideoTrack();

      // Auto-detect audio source.  Uses a local variable to avoid the
      // React async-state pitfall — setSelectedAudioAppId is async but
      // we need the resolved ID synchronously right here.
      let targetAudioId: number | null = selectedAudioAppId;

      if (targetAudioId === null) {
        const opts = isWayland
          ? { trackLabel: videoTrack.label }
          : { sourceId: selectedSourceId };
        const app = await attemptAutoResolve(opts);
        targetAudioId = app?.id ?? null;
      }

      if (targetAudioId === null) {
        setStatusMsg('Could not detect audio source. Select one manually.');
        videoTrack.stop();
        return;
      }

      const audioTrack = await captureAudioTrack(targetAudioId);

      const tracks = audioTrack ? [videoTrack, audioTrack] : [videoTrack];
      const stream = new MediaStream(tracks);
      localStreamRef.current = stream;
      setPreviewStream(stream);

      videoTrack.onended = () => {
        handleStopShare();
      };

      setIsSharing(true);
      isSharingRef.current = true;
      setStatusMsg('Screenshare streaming live (window audio only)!');

      if (wsRef.current && wsRef.current.readyState === WebSocket.OPEN) {
        wsRef.current.send(
          JSON.stringify({
            type: 'PUBLISH_STREAM',
            payload: { streamId: 'desktop-main-display' },
          })
        );
      }
    } catch (err: any) {
      console.error('Failed to capture screen:', err);
      setStatusMsg('Capture error: ' + err.message);
      if (window.electronAPI) {
        await window.electronAPI.stopAudioCapture();
      }
    }
  };

  const handleStopShare = async () => {
    const stream = localStreamRef.current;
    if (stream) {
      stream.getTracks().forEach((track) => track.stop());
      localStreamRef.current = null;
    }
    setPreviewStream(null);
    peerConnectionsRef.current.forEach((pc) => pc.close());
    peerConnectionsRef.current.clear();
    pendingCandidatesRef.current.clear();
    isSharingRef.current = false;
    if (window.electronAPI) {
      await window.electronAPI.stopAudioCapture();
    }
    setIsSharing(false);
    setStatusMsg('Screenshare stopped');
  };

  const flashCopied = (kind: 'link' | 'code') => {
    setCopied(kind);
    setTimeout(() => setCopied(null), 2000);
  };

  const handleCopyLink = async () => {
    const url = shareUrl || (roomCode ? `http://localhost:3000/room/${roomCode}` : '');
    if (!url) return;
    const ok = await copyText(url);
    if (ok) {
      flashCopied('link');
      setStatusMsg('Room link copied to clipboard');
    } else {
      setStatusMsg('Failed to copy room link');
    }
  };

  const handleCopyCode = async () => {
    if (!roomCode) return;
    const ok = await copyText(roomCode);
    if (ok) {
      flashCopied('code');
      setStatusMsg('Room code copied to clipboard');
    } else {
      setStatusMsg('Failed to copy room code');
    }
  };

  const canStartShare =
    !!roomCode && !isSharing && (isWayland || (!!selectedSourceId && selectedAudioAppId !== null));

  return (
    <div className="p-6 max-w-5xl mx-auto space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-gray-800 pb-4 gap-4">
        <div className="min-w-0">
          <h1 className="text-2xl font-bold text-white flex items-center gap-2 flex-wrap">
            <span>Desktop Presenter Studio</span>
            <span className="text-xs bg-indigo-500/20 text-indigo-400 px-2.5 py-0.5 rounded-full border border-indigo-500/30">
              {isWayland ? 'Wayland Portal' : 'PipeWire Native'}
            </span>
            {isSharing && (
              <span className="text-xs bg-rose-500/20 text-rose-300 px-2.5 py-0.5 rounded-full border border-rose-500/30 animate-pulse">
                LIVE
              </span>
            )}
          </h1>
          <p className="text-xs text-gray-400 mt-1 truncate">Status: {statusMsg}</p>
        </div>

        {!roomCode ? (
          <button
            onClick={handleCreateRoom}
            className="shrink-0 px-5 py-2.5 bg-indigo-600 hover:bg-indigo-500 text-white rounded-lg font-semibold text-sm transition-all shadow-lg"
          >
            Create Live Room
          </button>
        ) : (
          <div className="shrink-0 flex flex-col items-end gap-1.5">
            <div className="flex items-center gap-2 bg-gray-900 border border-gray-800 px-3 py-2 rounded-xl">
              <span className="text-xs text-gray-400 font-mono">Room:</span>
              <button
                type="button"
                onClick={handleCopyCode}
                title="Copy room code"
                className="text-sm font-mono font-bold text-indigo-400 hover:text-indigo-300"
              >
                {roomCode}
              </button>
              <button
                type="button"
                onClick={handleCopyCode}
                className="text-xs bg-gray-800 hover:bg-gray-700 text-gray-200 px-2.5 py-1 rounded-lg border border-gray-700"
              >
                {copied === 'code' ? 'Code Copied!' : 'Copy Code'}
              </button>
              <button
                type="button"
                onClick={handleCopyLink}
                className="text-xs bg-indigo-600 hover:bg-indigo-500 text-white px-2.5 py-1 rounded-lg"
              >
                {copied === 'link' ? 'Link Copied!' : 'Copy Link'}
              </button>
            </div>
            {shareUrl && (
              <p className="text-[10px] text-gray-500 font-mono max-w-xs truncate" title={shareUrl}>
                {shareUrl}
              </p>
            )}
          </div>
        )}
      </div>

      {/* Live Preview */}
      <div className="bg-gray-900/80 border border-gray-800 rounded-xl overflow-hidden">
        <div className="flex items-center justify-between px-4 py-2.5 border-b border-gray-800">
          <h2 className="text-sm font-bold uppercase tracking-wider text-gray-200">
            Screenshare Preview
          </h2>
          <div className="flex items-center gap-3 text-xs text-gray-400">
            <span>
              Spectators:{' '}
              <span className="text-gray-200 font-semibold">{spectatorCount}</span>
            </span>
            <span
              className={`inline-flex items-center gap-1.5 ${
                isSharing ? 'text-emerald-400' : 'text-gray-500'
              }`}
            >
              <span
                className={`w-1.5 h-1.5 rounded-full ${
                  isSharing ? 'bg-emerald-400' : 'bg-gray-600'
                }`}
              />
              {isSharing ? 'Broadcasting' : 'Idle'}
            </span>
          </div>
        </div>
        <div className="relative bg-black aspect-video flex items-center justify-center">
          <video
            ref={previewVideoRef}
            autoPlay
            playsInline
            muted
            className={`w-full h-full object-contain ${isSharing ? 'block' : 'hidden'}`}
          />
          {!isSharing && (
            <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 text-center px-6">
              <p className="text-sm text-gray-400 font-medium">No active screenshare</p>
              <p className="text-xs text-gray-600 max-w-sm">
                Pick a window (X11 thumbnail or Wayland portal) and start screenshare. Audio is
                auto-detected — no separate selection needed.
              </p>
            </div>
          )}
          {isSharing && (
            <div className="absolute bottom-3 left-3 text-[10px] uppercase tracking-wider bg-black/70 text-gray-300 px-2 py-1 rounded border border-white/10">
              Local preview · audio muted
            </div>
          )}
        </div>
      </div>

      {/* Main Grid */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-6">
        {/* Window Audio Target Panel */}
        <div className="bg-gray-900/80 border border-gray-800 rounded-xl p-5 space-y-4">
          <div className="flex items-center justify-between">
            <h2 className="text-sm font-bold uppercase tracking-wider text-gray-200 flex items-center gap-2">
              <span>Window Audio Capture</span>
              {autoDetectedApp && (
                <span className="text-[10px] font-normal text-indigo-300 bg-indigo-500/15 px-2 py-0.5 rounded-full border border-indigo-500/25">
                  Auto ✓
                </span>
              )}
            </h2>
            <button
              onClick={loadAudioApps}
              className="text-xs text-indigo-400 hover:underline"
            >
              Refresh Apps
            </button>
          </div>

          <p className="text-xs text-gray-400">
            When you pick a window (via the thumbnail grid or the Wayland portal dialog), the
            correct audio source is <em>auto-detected</em> — no manual selection needed.
            <br />
            Click an app below to override auto-detection. ONLY the selected app's audio is
            streamed; all other audio stays private.
          </p>

          <div className="space-y-2 max-h-56 overflow-y-auto pr-1">
            {audioApps.length === 0 ? (
              <p className="text-xs text-gray-500 text-center py-4">
                No active audio applications detected. If the window you are sharing produces
                sound, it will be auto-detected when available.
              </p>
            ) : (
              audioApps.map((app) => {
                const isSelected = app.id === selectedAudioAppId;
                const isAutoDetected = autoDetectedApp?.id === app.id;
                return (
                  <div
                    key={app.id}
                    onClick={() => {
                      setSelectedAudioAppId(app.id);
                      setAutoDetectedApp(null);
                    }}
                    className={`flex items-center justify-between p-3 rounded-lg border text-xs transition-colors cursor-pointer ${
                      isSelected
                        ? 'bg-emerald-950/40 border-emerald-500/40 text-emerald-200'
                        : 'bg-gray-950/60 border-gray-800 text-gray-300 hover:border-gray-700'
                    }`}
                  >
                    <div className="min-w-0">
                      <span className="font-semibold block truncate">{app.name}</span>
                      <div className="flex items-center gap-2 mt-0.5">
                        <span className="text-[10px] opacity-70">PID: {app.processId}</span>
                        {isAutoDetected && (
                          <span className="text-[10px] bg-indigo-500/20 text-indigo-300 px-2 py-0.5 rounded-full border border-indigo-500/30">
                            Auto-detected ✓
                          </span>
                        )}
                      </div>
                    </div>

                    <span
                      className={`shrink-0 px-3 py-1 rounded-md text-xs font-semibold ${
                        isSelected ? 'bg-emerald-600 text-white' : 'bg-gray-800 text-gray-400'
                      }`}
                    >
                      {isSelected ? (isAutoDetected ? 'Auto' : 'Selected') : 'Select'}
                    </span>
                  </div>
                );
              })
            )}
          </div>
        </div>

        {/* Display Capture Source Picker */}
        <div className="bg-gray-900/80 border border-gray-800 rounded-xl p-5 space-y-4">
          <h2 className="text-sm font-bold uppercase tracking-wider text-gray-200">
            Screenshare Window Source
          </h2>

          {isWayland ? (
            <div className="text-xs text-gray-400 space-y-2 py-2">
              <p>
                Your desktop is running Wayland. When you start the screenshare, the system's own
                dialog (xdg-desktop-portal) will let you pick the window to share.
              </p>
              <p className="text-gray-500">
                Only the window you pick there is streamed — nothing else on your screen.
              </p>
            </div>
          ) : (
            <div className="grid grid-cols-2 gap-3 max-h-56 overflow-y-auto pr-1">
              {desktopSources.map((source) => {
                const isSelected = source.id === selectedSourceId;
                return (
                  <div
                    key={source.id}
                    onClick={() => {
                      setSelectedSourceId(source.id);
                      void attemptAutoResolve({ sourceId: source.id });
                    }}
                    className={`p-2 rounded-lg border cursor-pointer transition-all text-xs text-center space-y-2 ${
                      isSelected
                        ? 'bg-indigo-950/60 border-indigo-500 ring-2 ring-indigo-500/30'
                        : 'bg-gray-950/60 border-gray-800 hover:border-gray-700'
                    }`}
                  >
                    <img
                      src={source.thumbnail}
                      alt={source.name}
                      className="w-full h-20 object-cover rounded"
                    />
                    <span className="block font-medium truncate text-gray-200">{source.name}</span>
                  </div>
                );
              })}
            </div>
          )}

          <button
            onClick={isSharing ? handleStopShare : handleStartShare}
            disabled={!isSharing && !canStartShare}
            className={`w-full py-3 text-white font-bold rounded-lg text-sm transition-all shadow-lg disabled:opacity-50 ${
              isSharing ? 'bg-rose-600 hover:bg-rose-500' : 'bg-emerald-600 hover:bg-emerald-500'
            }`}
          >
            {isSharing ? 'Stop Screenshare' : 'Start Screenshare'}
          </button>
        </div>
      </div>
    </div>
  );
};

const root = ReactDOM.createRoot(document.getElementById('root')!);
root.render(<PresenterApp />);
