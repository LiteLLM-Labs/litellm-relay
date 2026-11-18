import { randomBytes } from 'node:crypto';
import type { Gateway, LoginFlow } from './gateway.ts';
import type { StateStore } from './state.ts';
import { failure } from './protocol.ts';
import type { Result } from './protocol.ts';

type Pending = Readonly<{ flow: LoginFlow; expiresAt: number }>;

export class Authentication {
  private readonly flows = new Map<string, Pending>();
  private readonly polling = new Set<string>();
  private readonly gateway: Gateway;
  private readonly state: StateStore;
  private readonly gatewayUrl: string;
  private readonly now: () => number;

  constructor(gateway: Gateway, state: StateStore, gatewayUrl: string, now: () => number = Date.now) {
    this.gateway = gateway;
    this.state = state;
    this.gatewayUrl = gatewayUrl;
    this.now = now;
  }

  async start() {
    for (const [token, flow] of this.flows) if (flow.expiresAt <= this.now()) this.flows.delete(token);
    if (this.flows.size >= 200) return failure(429, 'Too many sign-in attempts. Please wait.');
    const started = await this.gateway.start();
    if (!started.ok) return started;
    const flowToken = randomBytes(32).toString('base64url');
    this.flows.set(flowToken, { flow: started.value, expiresAt: this.now() + Math.min(started.value.expires_in, 600) * 1000 });
    const url = new URL(`${this.gatewayUrl}/sso/key/generate`);
    url.searchParams.set('source', 'litellm-cli');
    url.searchParams.set('key', started.value.login_id);
    return { ok: true as const, value: { flowToken, code: started.value.user_code, verificationUrl: url.toString() } };
  }

  async poll(token: string, team?: string): Promise<Result<unknown>> {
    const pending = this.flows.get(token);
    if (!pending || pending.expiresAt <= this.now()) return failure(410, 'Sign-in expired. Please start again.');
    if (this.polling.has(token)) return failure(409, 'Sign-in is already being checked.');
    this.polling.add(token);
    try {
      const polled = await this.gateway.poll(pending.flow, team);
      if (!polled.ok) return polled;
      const data = polled.value;
      if (data.status !== 'ready') return { ok: true, value: { status: 'pending' } };
      if (data.requires_team_selection) return { ok: true, value: { status: 'teams',
        teams: (data.team_details ?? data.teams?.map(id => ({ team_id: id, team_alias: id })) ?? [])
          .map(entry => ({ id: entry.team_id, name: entry.team_alias || entry.team_id })),
      } };
      if (!data.user_id || !data.key) return failure(502, 'The gateway did not return a signed-in account.');
      const models = await this.gateway.models(data.key);
      this.flows.delete(token);
      return { ok: true, value: { status: 'ready', ...this.state.session(data.user_id, models.ok ? models.value : []) } };
    } finally { this.polling.delete(token); }
  }
}
