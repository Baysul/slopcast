import { loadConfig } from '@slopcast/shared-types/config';
import express from 'express';
import { rateLimit } from 'express-rate-limit';

import { initRoutes } from './routes.js';

const config = loadConfig();

const app = express();
app.set('trust proxy', 1);
app.use(express.json({ limit: '16kb' }));

const roomCreateLimiter = rateLimit({
  legacyHeaders: false,
  limit: 10,
  message: { error: 'Too many room creation requests, please try again later' },
  standardHeaders: true,
  windowMs: 60_000,
});

const spectatorTokenLimiter = rateLimit({
  legacyHeaders: false,
  limit: 30,
  message: { error: 'Too many token requests, please try again later' },
  standardHeaders: true,
  windowMs: 60_000,
});

const spectatorCountLimiter = rateLimit({
  legacyHeaders: false,
  limit: 120,
  message: { error: 'Too many spectator count requests, please try again later' },
  standardHeaders: true,
  windowMs: 60_000,
});

// Presenters heartbeat every ~20s; leave headroom for retries after a network blip.
const presenterTokenLimiter = rateLimit({
  legacyHeaders: false,
  limit: 30,
  message: { error: 'Too many presenter heartbeats, please try again later' },
  standardHeaders: true,
  windowMs: 60_000,
});

const allowedOrigins = new Set([
  config.websiteUrl,
  'http://localhost:3000',
  'http://127.0.0.1:3000',
  'http://[::1]:3000',
]);
app.use((req, res, next) => {
  const { origin } = req.headers;
  if (origin !== undefined && allowedOrigins.has(origin)) {
    res.setHeader('Access-Control-Allow-Origin', origin);
    res.setHeader('Access-Control-Allow-Methods', 'DELETE, GET, POST, OPTIONS');
    res.setHeader('Access-Control-Allow-Headers', 'Content-Type');
    res.setHeader('Vary', 'Origin');
    res.setHeader('Access-Control-Max-Age', '86400');
  }
  if (req.method === 'OPTIONS') {
    res.status(204).end();
    return;
  }
  next();
});

app.post('/api/rooms', roomCreateLimiter);
app.use('/api/rooms/:code/token', spectatorTokenLimiter);
app.use('/api/rooms/:code/spectators', spectatorCountLimiter);
app.use('/api/rooms/:code/presenter-token', presenterTokenLimiter);
let livekitClientUrl = process.env.LIVEKIT_CLIENT_URL;
if (livekitClientUrl === '') {
  livekitClientUrl = undefined;
}
app.use(
  initRoutes(config.livekitUrl, config.livekitApiKey, config.livekitApiSecret, config.websiteUrl, livekitClientUrl),
);

app.use((err: Error, _req: express.Request, res: express.Response, _next: express.NextFunction) => {
  console.error('Unhandled server error:', err);
  res.status(500).json({ error: 'Internal server error' });
});

app.listen(config.serverPort, () => {
  console.log(`Slopcast REST API listening on :${config.serverPort}`);
  console.log(`LiveKit server: ${config.livekitUrl}`);
});
