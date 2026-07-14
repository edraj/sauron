// Resolution order for the API base URL:
//   1. window.__SAURON_CONFIG__.apiBaseUrl  (runtime-injected in production)
//   2. import.meta.env.VITE_API_BASE_URL    (build-time override)
//   3. http://localhost:8090                (local dev default)
const runtime = typeof window !== 'undefined' ? window.__SAURON_CONFIG__ : undefined;

export const apiBaseUrl: string =
  runtime?.apiBaseUrl ?? import.meta.env.VITE_API_BASE_URL ?? 'http://localhost:8090';

// The ingest gateway is used to render app DSNs (http(s)://<key>@<host>/<app_id>).
// Dev default 8091; compose injects INGEST_BASE_URL (default 8081) at runtime.
export const ingestBaseUrl: string =
  runtime?.ingestBaseUrl ?? import.meta.env.VITE_INGEST_BASE_URL ?? 'http://localhost:8091';
