import { describe, expect, it } from 'vitest';
import { mcpAddCommand } from './mcp-command';

describe('mcpAddCommand', () => {
  it('builds the claude mcp add command for an absolute API base', () => {
    expect(mcpAddCommand('https://s.example.com/api', 'sauron_pat_x', 'https://d')).toBe(
      'claude mcp add --transport http sauron https://s.example.com/api/v1/mcp --header "Authorization: Bearer sauron_pat_x"',
    );
  });

  it('tolerates a trailing slash on the base', () => {
    expect(mcpAddCommand('https://s.example.com/api/', 't', 'https://d')).toContain(
      'https://s.example.com/api/v1/mcp ',
    );
  });

  it('resolves a relative base against the page origin', () => {
    expect(mcpAddCommand('/sauron-api', 't', 'https://h.example')).toContain(
      'https://h.example/sauron-api/v1/mcp ',
    );
  });
});
