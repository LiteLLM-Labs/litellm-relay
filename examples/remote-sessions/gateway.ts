import { z } from 'zod';
import { failure } from './protocol.ts';
import type { Result } from './protocol.ts';

const startSchema = z.object({
  login_id: z.string().min(1), poll_secret: z.string().min(1), user_code: z.string().min(1),
  expires_in: z.number().positive().default(600),
});
const pollSchema = z.object({
  status: z.string(), key: z.string().optional(), user_id: z.string().optional(),
  requires_team_selection: z.boolean().optional(), teams: z.array(z.string()).optional(),
  team_details: z.array(z.object({ team_id: z.string(), team_alias: z.string().nullish() })).nullish(),
});
export type LoginFlow = z.infer<typeof startSchema>;
export type LoginPoll = z.infer<typeof pollSchema>;
export interface Gateway {
  start(): Promise<Result<LoginFlow>>;
  poll(flow: LoginFlow, team?: string): Promise<Result<LoginPoll>>;
  verify(key: string): Promise<boolean>;
}

export class LiteLLMGateway implements Gateway {
  readonly url: string;
  constructor(url: string) { this.url = url.replace(/\/$/, ''); }

  async start(): Promise<Result<LoginFlow>> {
    try {
      const response = await fetch(`${this.url}/sso/cli/start`, { method: 'POST', redirect: 'error', signal: AbortSignal.timeout(10000) });
      if (!response.ok) return failure(502, `LiteLLM sign-in is unavailable (HTTP ${response.status}).`);
      const parsed = startSchema.safeParse(await response.json());
      return parsed.success ? { ok: true, value: parsed.data } : failure(502, 'The gateway does not support this sign-in flow.');
    } catch { return failure(502, 'Cannot reach your LiteLLM gateway. Check your network or VPN.'); }
  }

  async poll(flow: LoginFlow, team?: string): Promise<Result<LoginPoll>> {
    const url = new URL(`${this.url}/sso/cli/poll/${encodeURIComponent(flow.login_id)}`);
    if (team) url.searchParams.set('team_id', team);
    try {
      const response = await fetch(url, {
        headers: { 'x-litellm-cli-poll-secret': flow.poll_secret }, redirect: 'error', signal: AbortSignal.timeout(10000),
      });
      if (!response.ok) return failure(response.status === 404 ? 410 : 502, 'Sign-in expired or was refused. Please try again.');
      const parsed = pollSchema.safeParse(await response.json());
      return parsed.success ? { ok: true, value: parsed.data } : failure(502, 'The gateway returned an unsupported sign-in response.');
    } catch { return failure(502, 'Cannot reach your LiteLLM gateway.'); }
  }

  async verify(key: string): Promise<boolean> {
    try {
      const response = await fetch(`${this.url}/v1/models`, {
        headers: { Authorization: `Bearer ${key}` }, redirect: 'error', signal: AbortSignal.timeout(10000),
      });
      if (!response.ok) return false;
      return z.object({ data: z.array(z.unknown()) }).safeParse(await response.json()).success;
    } catch { return false; }
  }
}
