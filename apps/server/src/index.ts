import { loadConfig } from '@slopcast/shared-types/config';
import express from 'express';
import { rateLimit } from 'express-rate-limit';
import { initRoutes } from './routes.js';

const config = loadConfig();

const app = express();
app.set('trust proxy', 1);
app.use(express.json({ limit: '16kb' }));

const roomCreateLimiter = rateLimit({
  windowMs: 60_000,
  max: 10,
  standardHeaders: true,
  legacyHeaders: false,
  message: { error: 'Too many room creation requests, please try again later' },
});

const spectatorTokenLimiter = rateLimit({
  windowMs: 60_000,
  max: 30,
  standardHeaders: true,
  legacyHeaders: false,
  message: { error: 'Too many token requests, please try again later' },
});

const allowedOrigins = new Set([
  config.websiteUrl,
  'http://localhost:3000',
  'http://127.0.0.1:3000',
  'http://[::1]:3000',
  'http://localhost:5173',
  'http://127.0.0.1:5173',
  'http://[::1]:5173',
]);
app.use((req, res, next) => {
  const origin = req.headers.origin;
  if (origin && allowedOrigins.has(origin)) {
    res.setHeader('Access-Control-Allow-Origin', origin);
    res.setHeader('Access-Control-Allow-Methods', 'GET, POST, OPTIONS');
    res.setHeader('Access-Control-Allow-Headers', 'Content-Type, X-Client-Origin');
    res.setHeader('Vary', 'Origin');
    res.setHeader('Access-Control-Max-Age', '86400');
  }
  if (req.method === 'OPTIONS') {
    res.status(204).end();
    return;
  }
  next();
});

// Mounted on the POST path only: a prefix-mount would also rate-limit
// /api/rooms/:code/token, capping spectators at the create limit.
app.post('/api/rooms', roomCreateLimiter);
app.use('/api/rooms/:code/token', spectatorTokenLimiter);
app.use(
  initRoutes(
    config.livekitUrl,
    config.livekitApiKey,
    config.livekitApiSecret,
    config.websiteUrl,
    process.env.LIVEKIT_CLIENT_URL || undefined,
  ),
);

app.use((err: Error, _req: express.Request, res: express.Response, _next: express.NextFunction) => {
  console.error('Unhandled server error:', err);
  res.status(500).json({ error: 'Internal server error' });
});

app.listen(config.serverPort, () => {
  console.log(`Slopcast REST API listening on :${config.serverPort}`);
  console.log(`LiveKit server: ${config.livekitUrl}`);
});
