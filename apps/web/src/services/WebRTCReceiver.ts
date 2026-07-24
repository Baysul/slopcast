import type { SignalingClient } from './SignalingClient';

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
  private signalingClient: SignalingClient;
  private callbacks: WebRTCReceiverCallbacks;
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
      // biome-ignore lint/suspicious/noExplicitAny: WebRTC signal type is dynamic by protocol
      await this.handleSignal(senderId, signal as any);
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
        event.streams[0].getTracks().forEach((track) => {
          if (!this.mediaStream.getTracks().some((t) => t.id === track.id)) {
            this.mediaStream.addTrack(track);
          }
        });
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

  // biome-ignore lint/suspicious/noExplicitAny: WebRTC signal type is dynamic by protocol
  private async handleSignal(senderId: string, signal: any) {
    this.initializePeerConnection();
    if (!this.pc) return;

    try {
      if (signal?.type === 'offer') {
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

        // ── Set receiver codec preferences to favour H.264 (HW decode) ──
        try {
          const transceivers = this.pc.getTransceivers();
          const videoTransceiver = transceivers.find((t) => t.receiver?.track?.kind === 'video');
          if (videoTransceiver) {
            const caps = RTCRtpReceiver.getCapabilities('video');
            if (caps?.codecs?.length) {
              const codecOrder = ['VIDEO/H264', 'VIDEO/VP9', 'VIDEO/VP8'];

              const H264_HIGH = 0x64;
              const H264_MAIN = 0x4d;
              const H264_BASELINE = 0x42;
              const h264ProfileRank = (fmtp?: string): number => {
                if (!fmtp) return 99;
                const m = fmtp.match(/profile-level-id=([0-9a-fA-F]{6})/);
                if (!m) return 99;
                const profile = parseInt(m[1].slice(0, 2), 16);
                switch (profile) {
                  case H264_HIGH:
                    return 0;
                  case H264_MAIN:
                    return 1;
                  case H264_BASELINE:
                    return 2;
                  default:
                    return 3;
                }
              };

              const preferred = caps.codecs
                .filter((c) => codecOrder.includes(c.mimeType.toUpperCase()))
                .sort((a, b) => {
                  const ia = codecOrder.indexOf(a.mimeType.toUpperCase());
                  const ib = codecOrder.indexOf(b.mimeType.toUpperCase());
                  const da = ia === -1 ? 99 : ia;
                  const db = ib === -1 ? 99 : ib;
                  if (da !== db) return da - db;
                  if (a.mimeType.toUpperCase() === 'VIDEO/H264') {
                    return h264ProfileRank(a.sdpFmtpLine) - h264ProfileRank(b.sdpFmtpLine);
                  }
                  return 0;
                });

              // Deduplicate by MIME type so only one variant per codec
              // is passed — setCodecPreferences rejects entries that
              // don't match the offer's negotiated PTs.
              const seen = new Set<string>();
              const deduped = preferred.filter((c) => {
                const key = c.mimeType.toUpperCase();
                if (seen.has(key)) return false;
                seen.add(key);
                return true;
              });

              videoTransceiver.setCodecPreferences(deduped);
              console.log(
                '[WebRTCReceiver] codec prefs:',
                deduped
                  .map((c) => {
                    const plid = c.sdpFmtpLine?.match(/profile-level-id=([0-9a-fA-F]{6})/)?.[1];
                    return plid ? `${c.mimeType}(${plid})` : c.mimeType;
                  })
                  .join(' > '),
              );
            }
          }
        } catch (err) {
          console.warn('[WebRTCReceiver] setCodecPreferences:', err);
        }

        await this.flushPendingCandidates();

        const answer = await this.pc.createAnswer();
        await this.pc.setLocalDescription(answer);

        this.signalingClient.sendSignal(senderId, {
          type: 'answer',
          sdp: answer.sdp,
        });
        console.log('[WebRTCReceiver] Sent answer to presenter');
        this.handlingOffer = false;
      } else if (signal?.type === 'candidate' || signal?.candidate) {
        const candidateInit: RTCIceCandidateInit =
          signal.candidate && typeof signal.candidate === 'object' ? signal.candidate : signal;

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
