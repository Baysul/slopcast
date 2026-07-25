#!/bin/sh
set -e

cat > /app/config.js << EOF
window.__SLOPCAST_CONFIG__ = {
  apiEndpoint: "${API_ENDPOINT:-http://localhost:3001}"
};
EOF

exec serve -l "${WEB_PORT:-3000}" /app
