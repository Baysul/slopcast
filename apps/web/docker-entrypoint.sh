#!/bin/sh
set -e

node -e "const fs=require('fs');fs.writeFileSync('/app/config.js','window.__SLOPCAST_CONFIG__ = '+JSON.stringify({apiEndpoint:process.env.API_ENDPOINT||'http://localhost:3001'})+';')"

exec serve -l "${WEB_PORT:-3000}" /app
