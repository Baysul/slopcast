import { loadConfig } from '@slopcast/shared-types/config';
import { createServer } from './server';

const config = loadConfig();

const { server } = createServer(config.serverPort, config.websiteUrl);

server.listen(config.serverPort, () => {
  console.log(`Signaling & Room Server listening on :${config.serverPort}`);
  console.log(`WebSocket: ws://localhost:${config.serverPort}`);
  console.log(`Room share base URL: ${config.websiteUrl}`);
});
