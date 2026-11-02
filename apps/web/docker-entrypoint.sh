#!/bin/sh
set -e

node -e "const fs=require('fs');fs.writeFileSync('/app/config.js','window.slopcastConfig = '+JSON.stringify({apiEndpoint:process.env.API_ENDPOINT||'http://localhost:3001'})+';window.__SLOPCAST_CONFIG__ = window.slopcastConfig;')"

exec serve -s -l "${WEB_PORT:-3000}" /app
