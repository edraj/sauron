// Template consumed by docker-entrypoint.sh via `envsubst`.
// ${API_BASE_URL} and ${INGEST_BASE_URL} are substituted at container start.
window.__SAURON_CONFIG__ = {
  apiBaseUrl: '${API_BASE_URL}',
  ingestBaseUrl: '${INGEST_BASE_URL}',
};
