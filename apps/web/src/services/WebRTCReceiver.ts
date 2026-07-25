import type { SignalingClient } from './SignalingClient';

function getSignalType(signal: unknown): string | null {
  if (signal && typeof signal === 'object' && 'type' in signal) {
    const t = (signal as Record<string, unknown>).type;
    return typeof t === 'string' ? t : null;
  }
  return null;
}

export type WebRTCConnectionState = 'new' | 'connecting' | 'connected' | 'disconnected' | 'failed' | 'closed';

export interface WebRTCReceiverCallbacks {
  onStream: (stream: MediaStream) => void;
  onStateChange: (state: WebRTCConnectionState) => void;
  onError: (error: Error) => void;
  onPublishNotice?: (presenterId: string) => void;
  onStreamEnd?: () => void;
}

export class WebRTCReceiver {
  private pc: RTCPeerConnection | null = null;
  private readonly signalingClient: SignalingClient;
  private readonly callbacks: WebRTCReceiverCallbacks;
  private presenterId: string | null = null;
  private pendingCandidates: RTCIceCandidateInit[] = [];
  private mediaStream: MediaStream = new MediaStream();
  private handlingOffer = false;

  constructor(signalingClient: SignalingClient, callbacks: WebRTCReceiverCallbacks) {
    this.signalingClient = signalingClient;
    this.callbacks = callbacks;
    this.setupSignalingListeners();
  }

  private setupSignalingListeners() {
    this.signalingClient.on('webrtc_signal', async ({ senderId, signal }) => {
      this.presenterId = senderId;
      await this.handleSignal(senderId, signal);
    });

    this.signalingClient.on('publish_stream', ({ senderId }) => {
      this.presenterId = senderId;
      console.log(`[WebRTCReceiver] Presenter ${senderId} published stream, waiting for offer...`);
      this.callbacks.onStateChange('connecting');
      this.callbacks.onPublishNotice?.(senderId);
    });

    this.signalingClient.on('stop_stream', () => {
      console.log('[WebRTCReceiver] Stream stopped by presenter');
      this.close();
      this.callbacks.onStreamEnd?.();
    });
  }

  private initializePeerConnection() {
    if (this.pc) {
      return;
    }

    const config: RTCConfiguration = {
      iceServers: [{ urls: 'stun:stun.l.google.com:19302' }, { urls: 'stun:stun1.l.google.com:19302' }],
    };

    this.pc = new RTCPeerConnection(config);

    // (setCodecPreferences is applied per-transceiver after receiving the offer)

    this.pc.ontrack = (event) => {
      console.log('[WebRTCReceiver] Received remote track:', event.track.kind, event.track.id);
      if (event.streams?.[0]) {
        for (const track of event.streams[0].getTracks()) {
          if (!this.mediaStream.getTracks().some((t) => t.id === track.id)) {
            this.mediaStream.addTrack(track);
          }
        }
      } else if (!this.mediaStream.getTracks().some((t) => t.id === event.track.id)) {
        this.mediaStream.addTrack(event.track);
      }
      this.callbacks.onStream(this.mediaStream);
    };

    this.pc.onicecandidate = (event) => {
      if (event.candidate && this.presenterId) {
        this.signalingClient.sendSignal(this.presenterId, {
          type: 'candidate',
          candidate: event.candidate.toJSON(),
        });
      }
    };

    this.pc.onconnectionstatechange = () => {
      if (!this.pc) return;
      console.log('[WebRTCReceiver] Connection state:', this.pc.connectionState);
      this.callbacks.onStateChange(this.pc.connectionState as WebRTCConnectionState);
    };

    this.pc.oniceconnectionstatechange = () => {
      if (!this.pc) return;
      console.log('[WebRTCReceiver] ICE connection state:', this.pc.iceConnectionState);
      if (this.pc.iceConnectionState === 'failed' || this.pc.iceConnectionState === 'disconnected') {
        this.callbacks.onStateChange(this.pc.iceConnectionState as WebRTCConnectionState);
      }
    };
  }

  private async flushPendingCandidates() {
    if (!this.pc?.remoteDescription) return;
    while (this.pendingCandidates.length > 0) {
      const candidate = this.pendingCandidates.shift();
      if (candidate) {
        try {
          await this.pc.addIceCandidate(new RTCIceCandidate(candidate));
        } catch (err) {
          console.warn('[WebRTCReceiver] Failed to add queued ICE candidate:', err);
        }
      }
    }
  }

  private async handleSignal(senderId: string, signal: unknown) {
    this.initializePeerConnection();
    if (!this.pc) return;

    try {
      if (getSignalType(signal) === 'offer') {
        if (this.handlingOffer) return;
        this.handlingOffer = true;
        console.log('[WebRTCReceiver] Received offer from presenter', senderId);

        // New offer → reset PC if we already answered previously.
        if (this.pc.signalingState !== 'stable' || this.pc.remoteDescription) {
          this.pc.close();
          this.pc = null;
          this.mediaStream = new MediaStream();
          this.pendingCandidates = [];
          this.initializePeerConnection();
        }

        if (!this.pc) return;

        const desc =
          signal.sdp !== undefined
            ? { type: 'offer' as RTCSdpType, sdp: signal.sdp }
            : (signal as RTCSessionDescriptionInit);

        await this.pc.setRemoteDescription(new RTCSessionDescription(desc));

        await this.flushPendingCandidates();

        const answer = await this.pc.createAnswer();

        // Codec negotiation is answerer-driven (RFC 3264): the presenter
        // sends the first codec from its own offer that also appears in
        // our answer.  The desktop offer already lists H.264 first
        // (setCodecPreferences on the presenter), so spectators that can
        // decode H.264 automatically receive H.264.
        //
        // We deliberately do NOT inject H.264 into the answer here.  A
        // browser omits H.264 from its answer precisely because its engine
        // cannot decode it (e.g. Chromium builds without proprietary
        // codecs); Chromium's answerer never drops a decodable codec.
        // Forcing H.264's PT into the answer makes setLocalDescription
        // fail with "Failed to set local video description recv
        // parameters" — and even if it succeeded, the spectator would be
        // unable to decode the stream.  Answering naturally lets such
        // browsers fall back to VP9/VP8, which the presenter then encodes.
        await this.pc.setLocalDescription(answer);

        this.signalingClient.sendSignal(senderId, {
          type: 'answer',
          sdp: answer.sdp,
        });
        console.log('[WebRTCReceiver] Sent answer to presenter');
        this.handlingOffer = false;
      } else if (
        getSignalType(signal) === 'candidate' ||
        (signal && typeof signal === 'object' && 'candidate' in signal)
      ) {
        const signalObj = signal as Record<string, unknown>;
        const candidateInit: RTCIceCandidateInit =
          signalObj.candidate && typeof signalObj.candidate === 'object'
            ? (signalObj.candidate as RTCIceCandidateInit)
            : (signal as RTCIceCandidateInit);

        if (this.pc.remoteDescription) {
          await this.pc.addIceCandidate(new RTCIceCandidate(candidateInit));
        } else {
          this.pendingCandidates.push(candidateInit);
        }
      }
    } catch (err) {
      this.handlingOffer = false;
      console.error('[WebRTCReceiver] Error handling signal:', err);
      this.callbacks.onError(err instanceof Error ? err : new Error(String(err)));
    }
  }

  public async getStats(): Promise<RTCStatsReport | null> {
    if (!this.pc) return null;
    try {
      return await this.pc.getStats();
    } catch {
      return null;
    }
  }

  public close() {
    this.handlingOffer = false;
    if (this.pc) {
      this.pc.close();
      this.pc = null;
    }
    for (const track of this.mediaStream.getTracks()) {
      track.stop();
    }
    this.mediaStream = new MediaStream();
    this.pendingCandidates = [];
    this.presenterId = null;
  }
}
