import { loadConfig } from '@slopcast/shared-types/config';
import express from 'express';
import { initRoutes } from './routes.js';

const config = loadConfig();

const app = express();
app.use(express.json());

const allowedOrigins = new Set([config.websiteUrl, 'http://localhost:3000', 'http://localhost:5173']);
app.use((req, res, next) => {
  const origin = req.headers.origin;
  if (origin && allowedOrigins.has(origin)) {
    res.setHeader('Access-Control-Allow-Origin', origin);
    res.setHeader('Access-Control-Allow-Methods', 'GET, POST, OPTIONS');
    res.setHeader('Access-Control-Allow-Headers', 'Content-Type, X-Client-Origin');
  }
  if (req.method === 'OPTIONS') {
    res.status(204).end();
    return;
  }
  next();
});

app.use(initRoutes(config.livekitUrl, config.livekitApiKey, config.livekitApiSecret, config.websiteUrl));

app.listen(config.serverPort, () => {
  console.log(`Slopcast REST API listening on :${config.serverPort}`);
  console.log(`LiveKit server: ${config.livekitUrl}`);
});
