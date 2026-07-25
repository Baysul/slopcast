import assert from 'node:assert';
import type { WSMessage } from '@slopcast/shared-types';
import WebSocket from 'ws';
import { createServer } from './server';

class MessageQueue {
  private queue: WSMessage[] = [];
  private resolvers: ((msg: WSMessage) => void)[] = [];

  constructor(public ws: WebSocket) {
    this.ws.on('message', (data: WebSocket.RawData) => {
      try {
        const msg = JSON.parse(data.toString());
        if (this.resolvers.length > 0) {
          const resolve = this.resolvers.shift()!;
          resolve(msg);
        } else {
          this.queue.push(msg);
        }
      } catch (err) {
        console.error('Failed to parse WS message:', err);
      }
    });
  }

  public getNextMessage<T = any>(): Promise<WSMessage<T>> {
    if (this.queue.length > 0) {
      return Promise.resolve(this.queue.shift()! as WSMessage<T>);
    }
    return new Promise((resolve) => {
      this.resolvers.push(resolve as (msg: WSMessage) => void);
    });
  }

  public send(msg: WSMessage) {
    this.ws.send(JSON.stringify(msg));
  }

  public close() {
    this.ws.close();
  }
}

async function runVerification() {
  console.log('🧪 Starting Signaling Server Verification Test...');

  const PORT = 3099;
  const { server } = createServer(PORT, `http://localhost:${PORT}`);

  await new Promise<void>((resolve) => server.listen(PORT, resolve));
  console.log(`Server listening on port ${PORT}`);

  const wsUrl = `ws://localhost:${PORT}`;

  try {
    // 1. Desktop Presenter Client
    const rawDesktopWs = new WebSocket(wsUrl, {
      headers: { 'x-client-origin': 'desktop' },
    });

    await new Promise<void>((resolve) => rawDesktopWs.on('open', resolve));
    const desktopClient = new MessageQueue(rawDesktopWs);

    let roomCode = '';

    // Create Room
    desktopClient.send({
      type: 'CREATE_ROOM',
      payload: { clientOrigin: 'desktop' },
    });

    const roomCreatedMsg = await desktopClient.getNextMessage();
    assert.strictEqual(roomCreatedMsg.type, 'ROOM_CREATED');
    assert.ok(roomCreatedMsg.payload.code);
    assert.strictEqual(roomCreatedMsg.payload.role, 'presenter');
    roomCode = roomCreatedMsg.payload.code;
    console.log(`✅ Room Created: ${roomCode} (${roomCreatedMsg.payload.shareUrl})`);

    const presenterRoleMsg = await desktopClient.getNextMessage();
    assert.strictEqual(presenterRoleMsg.type, 'ROLE_ASSIGNMENT');
    assert.strictEqual(presenterRoleMsg.payload.role, 'presenter');
    console.log('✅ Desktop client assigned presenter role');

    // 2. Web Spectator Client
    const rawWebWs = new WebSocket(wsUrl);
    await new Promise<void>((resolve) => rawWebWs.on('open', resolve));
    const webClient = new MessageQueue(rawWebWs);

    // Try joining with web origin requesting presenter role
    webClient.send({
      type: 'JOIN_ROOM',
      payload: {
        code: roomCode,
        clientOrigin: 'web',
        requestedRole: 'presenter',
      },
    });

    const joinedMsg = await webClient.getNextMessage();
    assert.strictEqual(joinedMsg.type, 'JOINED_ROOM');
    assert.strictEqual(joinedMsg.payload.role, 'spectator');

    const webRoleMsg = await webClient.getNextMessage();
    assert.strictEqual(webRoleMsg.type, 'ROLE_ASSIGNMENT');
    assert.strictEqual(webRoleMsg.payload.role, 'spectator');
    assert.ok(webRoleMsg.payload.reason.includes('Web clients are restricted'));
    console.log(`✅ Web client forced to spectator mode with guardrail notice: "${webRoleMsg.payload.reason}"`);

    // Consume USER_JOINED notification on desktop presenter
    const userJoinedMsg = await desktopClient.getNextMessage();
    assert.strictEqual(userJoinedMsg.type, 'USER_JOINED');
    console.log('✅ Presenter received USER_JOINED notification');

    // 3. Web Client Role Enforcement Guardrail: Publish Stream Rejection
    webClient.send({
      type: 'PUBLISH_STREAM',
      payload: { streamId: 'fake-web-stream' },
    });

    const publishRejectedMsg = await webClient.getNextMessage();
    assert.strictEqual(publishRejectedMsg.type, 'PUBLISH_REJECTED');
    console.log(`✅ Web client PUBLISH_STREAM rejected: "${publishRejectedMsg.payload.reason}"`);

    // 4. Web Client Role Enforcement Guardrail: WebRTC Offer Rejection
    webClient.send({
      type: 'WEBRTC_SIGNAL',
      payload: {
        signal: { type: 'offer', sdp: 'v=0...' },
      },
    });

    const offerRejectedMsg = await webClient.getNextMessage();
    assert.strictEqual(offerRejectedMsg.type, 'PUBLISH_REJECTED');
    console.log(`✅ Web client WebRTC Offer rejected: "${offerRejectedMsg.payload.reason}"`);

    // 5. Desktop Presenter Publishes Stream & Relays Offer
    desktopClient.send({
      type: 'PUBLISH_STREAM',
      payload: { streamId: 'desktop-display-1' },
    });

    const spectatorPublishRecv = await webClient.getNextMessage();
    assert.strictEqual(spectatorPublishRecv.type, 'PUBLISH_STREAM');
    assert.strictEqual(spectatorPublishRecv.payload.streamId, 'desktop-display-1');
    console.log('✅ Desktop PUBLISH_STREAM relayed to spectator');

    desktopClient.send({
      type: 'WEBRTC_SIGNAL',
      payload: {
        signal: { type: 'offer', sdp: 'v=0...presenter-offer' },
      },
    });

    const spectatorSignalRecv = await webClient.getNextMessage();
    assert.strictEqual(spectatorSignalRecv.type, 'WEBRTC_SIGNAL');
    assert.strictEqual(spectatorSignalRecv.payload.signal.type, 'offer');
    console.log('✅ Presenter WEBRTC_SIGNAL (offer) relayed to spectator');

    // 5a. Presenter stops streaming — spectator must be notified
    desktopClient.send({
      type: 'STOP_STREAM',
      payload: {},
    });

    const stopStreamMsg = await webClient.getNextMessage();
    assert.strictEqual(stopStreamMsg.type, 'STOP_STREAM');
    console.log('✅ STOP_STREAM relayed to spectator');

    // Consume PUBLISH_ACK that was sent to presenter when PUBLISH_STREAM was published
    const publishAckMsg = await desktopClient.getNextMessage();
    assert.strictEqual(publishAckMsg.type, 'PUBLISH_ACK');
    console.log('✅ Presenter received PUBLISH_ACK');

    // 6. Spectator sends Answer signal
    webClient.send({
      type: 'WEBRTC_SIGNAL',
      payload: {
        signal: { type: 'answer', sdp: 'v=0...spectator-answer' },
      },
    });

    const presenterSignalRecv = await desktopClient.getNextMessage();
    assert.strictEqual(presenterSignalRecv.type, 'WEBRTC_SIGNAL');
    assert.strictEqual(presenterSignalRecv.payload.signal.type, 'answer');
    console.log('✅ Spectator WEBRTC_SIGNAL (answer) relayed to presenter');

    // 7. Presenter disconnects -> Room Teardown
    desktopClient.close();

    const roomClosedMsg = await webClient.getNextMessage();
    assert.strictEqual(roomClosedMsg.type, 'ROOM_CLOSED');
    console.log(`✅ Room teardown broadcast to spectator upon presenter exit: "${roomClosedMsg.payload.reason}"`);

    webClient.close();
    console.log('🎉 All Signaling & Role Enforcement tests PASSED!');
  } finally {
    server.close();
  }
}

runVerification().catch((err) => {
  console.error('❌ Verification failed:', err);
  process.exit(1);
});
