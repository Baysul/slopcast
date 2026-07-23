import { createServer } from './server';

const PORT = process.env.PORT ? parseInt(process.env.PORT, 10) : 3001;
// Share links must point at the web spectator app (port 3000), not this server.
const BASE_URL = process.env.BASE_URL || 'http://localhost:3000';

const { server } = createServer(PORT, BASE_URL);

server.listen(PORT, () => {
  console.log(`🚀 Signaling & Room Server listening on :${PORT}`);
  console.log(`WebSocket: ws://localhost:${PORT}`);
  console.log(`Room share base URL: ${BASE_URL}`);
});

