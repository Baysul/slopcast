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

        // ── Original codec preference attempt (see SDP munging below) ──
        // setCodecPreferences on the answerer doesn't reliably select H.264:
        // Chromium requires exact codec object matches (including
        // sdpFmtpLine) and the spectator's preferred variant may not match
        // the presenter's offered variant.  We instead munge the answer
        // SDP to put H.264's payload type first in the m=video line (RFC
        // 3264: the first PT in the answer is the negotiated codec).

        await this.flushPendingCandidates();

        const answer = await this.pc.createAnswer();

        // Chromium's answerer drops H.264 from the answer entirely when
        // VP9/VP8 are also offered — even though the offer includes H.264
        // first and the receiver supports it.  We inject H.264's PT,
        // rtpmap, and fmtp lines from the OFFER into the ANSWER so the
        // negotiated codec becomes H.264 (RFC 3264: first PT in the
        // answer wins).
        const offerSdp = this.pc.remoteDescription?.sdp ?? '';
        const offerLines = offerSdp.split(/\r?\n/);

        // Find H.264 PT and its attribute lines in the offer's video section.
        const offerVideoStart = offerLines.findIndex((l) => l.startsWith('m=video'));
        let offerVideoEnd = offerLines.findIndex((l, i) => i > offerVideoStart && l.startsWith('m='));
        if (offerVideoEnd === -1) offerVideoEnd = offerLines.length;

        const offerVideoLines = offerLines.slice(offerVideoStart, offerVideoEnd);
        const h264Rtpmap = offerVideoLines.find((l) => /^a=rtpmap:\d+ H264\//.test(l));
        const h264Pt = h264Rtpmap?.match(/^a=rtpmap:(\d+)/)?.[1];

        const aLines = answer.sdp ?? '';
        let mungedSdp = aLines;
        if (h264Pt) {
          const ansLines = aLines.split(/\r?\n/);
          const ansMVideoIdx = ansLines.findIndex((l) => l.startsWith('m=video'));
          if (ansMVideoIdx !== -1) {
            // Find where to inject H.264's a= lines: after the last a=rtpmap in the video section.
            const ansVideoStart = ansMVideoIdx;
            let ansVideoEnd = ansLines.findIndex((l, i) => i > ansVideoStart && l.startsWith('m='));
            if (ansVideoEnd === -1) ansVideoEnd = ansLines.length;

            // Copy H.264's a=rtpmap, a=fmtp, a=rtcp-fb lines from the offer.
            const h264AttrLines = offerVideoLines.filter((l) =>
              new RegExp(`^a=(rtpmap|fmtp|rtcp-fb):${h264Pt}[ ]`).test(l),
            );

            // Inject H.264 PT at front of m=video line.
            const mParts = ansLines[ansMVideoIdx].split(' ');
            const pts = mParts.slice(3);
            const reordered = [h264Pt, ...pts.filter((p) => p !== h264Pt)];
            ansLines[ansMVideoIdx] = [...mParts.slice(0, 3), ...reordered].join(' ');

            // Inject a=rtpmap/a=fmtp/a=rtcp-fb before the next m= line.
            ansLines.splice(ansVideoEnd, 0, ...h264AttrLines);

            mungedSdp = ansLines.join('\r\n');
            console.log(
              `[WebRTCReceiver] injected H.264 PT ${h264Pt} into answer, attrs: ${h264AttrLines.length} lines`,
            );
          }
        } else {
          console.log('[WebRTCReceiver] no H.264 PT found in offer');
        }

        await this.pc.setLocalDescription({ type: 'answer', sdp: mungedSdp });

        this.signalingClient.sendSignal(senderId, {
          type: 'answer',
          sdp: mungedSdp,
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
