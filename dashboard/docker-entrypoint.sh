#!/bin/sh
set -eu

# Inject runtime configuration into config.js from the template.
# API_BASE_URL / INGEST_BASE_URL come from the container environment (compose).
: "${API_BASE_URL:=http://localhost:8080}"
: "${INGEST_BASE_URL:=http://localhost:8081}"
export API_BASE_URL INGEST_BASE_URL

TEMPLATE=/usr/share/nginx/html/config.template.js
OUTPUT=/usr/share/nginx/html/config.js

if [ -f "$TEMPLATE" ]; then
  envsubst '${API_BASE_URL} ${INGEST_BASE_URL}' < "$TEMPLATE" > "$OUTPUT"
  echo "[entrypoint] config.js generated: API_BASE_URL=${API_BASE_URL} INGEST_BASE_URL=${INGEST_BASE_URL}"
else
  echo "[entrypoint] WARNING: $TEMPLATE not found; leaving existing config.js in place" >&2
fi

exec "$@"
