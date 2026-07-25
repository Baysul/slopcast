import { loadConfig } from '@slopcast/shared-types/config';
import express from 'express';
import { initRoutes } from './routes.js';

const config = loadConfig();

const app = express();
app.use(express.json());
app.use(initRoutes(config.livekitUrl, config.livekitApiKey, config.livekitApiSecret, config.websiteUrl));

app.listen(config.serverPort, () => {
  console.log(`Slopcast REST API listening on :${config.serverPort}`);
  console.log(`LiveKit server: ${config.livekitUrl}`);
});
