/// <reference types="svelte" />
/// <reference types="vite/client" />

interface SauronRuntimeConfig {
  apiBaseUrl?: string;
  ingestBaseUrl?: string;
}

interface ImportMetaEnv {
  readonly VITE_API_BASE_URL?: string;
  readonly VITE_INGEST_BASE_URL?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}

interface Window {
  __SAURON_CONFIG__?: SauronRuntimeConfig;
}
