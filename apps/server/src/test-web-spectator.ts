import assert from 'node:assert';
import type { WSMessage } from '@slopcast/shared-types';
import WebSocket from 'ws';
import { createServer } from './server';

class TestWebClient {
  private ws: WebSocket;
  private messageQueue: WSMessage[] = [];
  private resolvers: ((msg: WSMessage) => void)[] = [];

  constructor(url: string) {
    this.ws = new WebSocket(url);
    this.ws.on('message', (data: WebSocket.RawData) => {
      try {
        const msg = JSON.parse(data.toString());
        if (this.resolvers.length > 0) {
          const resolve = this.resolvers.shift()!;
          resolve(msg);
        } else {
          this.messageQueue.push(msg);
        }
      } catch (err) {
        console.error('Failed to parse WS message:', err);
      }
    });
  }

  public connect(): Promise<void> {
    return new Promise((resolve, reject) => {
      this.ws.on('open', resolve);
      this.ws.on('error', reject);
    });
  }

  public getNextMessage<T>(): Promise<WSMessage<T>> {
    if (this.messageQueue.length > 0) {
      return Promise.resolve(this.messageQueue.shift()! as WSMessage<T>);
    }
    return new Promise((resolve) => {
      this.resolvers.push(resolve as (msg: WSMessage) => void);
    });
  }

  public joinRoom(code: string) {
    this.ws.send(
      JSON.stringify({
        type: 'JOIN_ROOM',
        payload: {
          code,
          clientOrigin: 'web',
          requestedRole: 'spectator',
        },
      }),
    );
  }

  public close() {
    this.ws.close();
  }
}

async function runWebClientTest() {
  console.log('🧪 Starting Web Spectator Integration Test...');

  const PORT = 3097;
  const { server } = createServer(PORT, `http://localhost:${PORT}`);

  await new Promise<void>((resolve) => server.listen(PORT, resolve));
  console.log(`Test Signaling Server listening on port ${PORT}`);

  try {
    // 1. Create a Desktop Presenter connection directly using WS
    const desktopWs = new WebSocket(`ws://localhost:${PORT}`, {
      headers: { 'x-client-origin': 'desktop' },
    });
    await new Promise<void>((resolve) => desktopWs.on('open', resolve));

    desktopWs.send(
      JSON.stringify({
        type: 'CREATE_ROOM',
        payload: { clientOrigin: 'desktop' },
      }),
    );

    const roomCreatedMsg = await new Promise<WSMessage>((resolve) => {
      desktopWs.once('message', (data) => resolve(JSON.parse(data.toString())));
    });
    const roomCode = roomCreatedMsg.payload.code;
    assert.ok(roomCode, 'Room code should be defined');
    console.log(`✅ Desktop Presenter created room: ${roomCode}`);

    // 2. Instantiate Web Spectator Client
    const webClient = new TestWebClient(`ws://localhost:${PORT}`);
    await webClient.connect();
    webClient.joinRoom(roomCode);

    const joinedMsg = await webClient.getNextMessage();
    assert.strictEqual(joinedMsg.type, 'JOINED_ROOM');
    assert.strictEqual(joinedMsg.payload.code, roomCode);
    assert.strictEqual(joinedMsg.payload.role, 'spectator');
    console.log('✅ Web Spectator joined room successfully as spectator');

    const roleMsg = await webClient.getNextMessage();
    assert.strictEqual(roleMsg.type, 'ROLE_ASSIGNMENT');
    assert.strictEqual(roleMsg.payload.role, 'spectator');
    console.log(`✅ Web Spectator received role assignment: ${roleMsg.payload.role}`);

    // 3. Desktop Presenter publishes stream
    desktopWs.send(
      JSON.stringify({
        type: 'PUBLISH_STREAM',
        payload: { streamId: 'desktop-screen-1' },
      }),
    );

    const publishMsg = await webClient.getNextMessage();
    assert.strictEqual(publishMsg.type, 'PUBLISH_STREAM');
    assert.strictEqual(publishMsg.payload.streamId, 'desktop-screen-1');
    console.log('✅ Web Spectator received PUBLISH_STREAM notice');

    // 3a. Desktop Presenter stops streaming — spectator must be notified
    desktopWs.send(
      JSON.stringify({
        type: 'STOP_STREAM',
        payload: {},
      }),
    );

    const stopStreamMsg = await webClient.getNextMessage();
    assert.strictEqual(stopStreamMsg.type, 'STOP_STREAM');
    console.log('✅ Web Spectator received STOP_STREAM notice');

    // 4. Desktop Presenter leaves -> Room Closed
    desktopWs.close();

    const roomClosedMsg = await webClient.getNextMessage();
    assert.strictEqual(roomClosedMsg.type, 'ROOM_CLOSED');
    console.log(`✅ Web Spectator received ROOM_CLOSED: ${roomClosedMsg.payload.reason}`);

    webClient.close();
    console.log('🎉 Web Spectator Integration Test PASSED!');
  } finally {
    server.close();
  }
}

runWebClientTest().catch((err) => {
  console.error('❌ Web Spectator Test failed:', err);
  process.exit(1);
});
