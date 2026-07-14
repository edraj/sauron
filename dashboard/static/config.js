// Runtime configuration for local dev.
// In production this file is generated from config.template.js by docker-entrypoint.sh
// (envsubst injects ${API_BASE_URL} / ${INGEST_BASE_URL}). It is served with no-cache so
// ops can change the backend URLs without rebuilding the bundle.
window.__SAURON_CONFIG__ = {
  apiBaseUrl: 'http://localhost:8090',
  ingestBaseUrl: 'http://localhost:8091',
};
