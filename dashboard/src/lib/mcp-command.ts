/**
 * The `claude mcp add` command a developer pastes after creating an API key.
 *
 * Pure so the vitest suite can import it. Deliberately NOT an i18n string: it is
 * a shell command, identical in every language.
 *
 * `apiBaseUrl` may be relative (`/sauron-api` behind a reverse proxy), so it is
 * resolved against the page `origin`; the trailing slash is forced so `new URL`
 * appends `v1/mcp` instead of replacing the last path segment.
 */
export function mcpAddCommand(apiBaseUrl: string, token: string, origin: string): string {
  const base = apiBaseUrl.endsWith('/') ? apiBaseUrl : `${apiBaseUrl}/`;
  const url = new URL('v1/mcp', new URL(base, origin)).href;
  return `claude mcp add --transport http sauron ${url} --header "Authorization: Bearer ${token}"`;
}
