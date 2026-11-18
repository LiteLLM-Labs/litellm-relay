import { randomBytes } from 'node:crypto';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { dirname } from 'node:path';
import { z } from 'zod';
import { failure } from './protocol.ts';
import type { Identity, Result } from './protocol.ts';
import type { Gateway, LoginFlow } from './gateway.ts';

type Pending = Readonly<{ flow: LoginFlow; expiresAt: number; canPair: boolean }>;
export type PollOutcome = Readonly<
  { status: 'pending' } |
  { status: 'teams'; teams: readonly Readonly<{ id: string; name: string }>[] } |
  { status: 'ready'; token: string; identity: Identity }
>;

export class Authentication {
  private readonly gateway: Gateway;
  private readonly gatewayKey: string;
  private readonly ownerFile: string;
  private readonly now: () => number;
  private owner: string | null = null;
  private readonly flows = new Map<string, Pending>();
  private readonly sessions = new Map<string, Identity>();

  constructor(gateway: Gateway, gatewayKey: string, ownerFile: string, now: () => number = Date.now) {
    this.gateway = gateway;
    this.gatewayKey = gatewayKey;
    this.ownerFile = ownerFile;
    this.now = now;
  }

  async load(): Promise<void> {
    try {
      const parsed = z.object({ subject: z.string().min(1) }).safeParse(JSON.parse(await readFile(this.ownerFile, 'utf8')));
      if (parsed.success) this.owner = parsed.data.subject;
    } catch (error) {
      if (error instanceof Error && 'code' in error && error.code === 'ENOENT') return;
      this.owner = '__owner_file_unreadable__';
    }
  }

  get paired(): boolean { return this.owner !== null; }
  get hasLocalConnection(): boolean { return Boolean(this.gatewayKey); }

  private prune(): void {
    for (const [id, flow] of this.flows) if (flow.expiresAt <= this.now()) this.flows.delete(id);
    for (const [id, identity] of this.sessions) if (identity.expiresAt <= this.now()) this.sessions.delete(id);
  }

  identity(token: string | undefined): Identity | null {
    this.prune();
    return token ? this.sessions.get(token) ?? null : null;
  }

  logout(token: string | undefined): void { if (token) this.sessions.delete(token); }

  private session(subject: string, method: Identity['method']): { token: string; identity: Identity } {
    const identity: Identity = { subject, label: method === 'sso' ? subject : 'This Mac’s gateway connection', method, expiresAt: this.now() + 8 * 3600000 };
    const token = randomBytes(32).toString('base64url');
    this.sessions.set(token, identity);
    return { token, identity };
  }

  async start(local: boolean): Promise<Result<Readonly<{ token: string; code: string }>>> {
    this.prune();
    if (!this.owner && !local) return failure(403, 'Pair this Mac first by signing in from its localhost address.');
    if (this.flows.size >= 20) return failure(429, 'Too many sign-in attempts. Please wait.');
    const started = await this.gateway.start();
    if (!started.ok) return started;
    const token = randomBytes(32).toString('base64url');
    this.flows.set(token, { flow: started.value, expiresAt: this.now() + Math.min(started.value.expires_in, 600) * 1000, canPair: local });
    return { ok: true, value: { token, code: started.value.user_code } };
  }

  verificationUrl(token: string, gatewayUrl: string): string | null {
    const pending = this.flows.get(token);
    if (!pending) return null;
    const url = new URL(`${gatewayUrl}/sso/key/generate`);
    url.searchParams.set('source', 'litellm-cli');
    url.searchParams.set('key', pending.flow.login_id);
    return url.toString();
  }

  async poll(token: string | undefined, team?: string): Promise<Result<PollOutcome>> {
    this.prune();
    const pending = token ? this.flows.get(token) : undefined;
    if (!pending) return failure(410, 'Sign-in expired. Please start again.');
    const polled = await this.gateway.poll(pending.flow, team);
    if (!polled.ok) return polled;
    const data = polled.value;
    if (data.status !== 'ready') return { ok: true, value: { status: 'pending' } };
    if (data.requires_team_selection) return {
      ok: true, value: { status: 'teams', teams: (data.team_details ?? data.teams?.map(id => ({ team_id: id, team_alias: id })) ?? [])
        .map(entry => ({ id: entry.team_id, name: entry.team_alias || entry.team_id })) },
    };
    if (!data.user_id || !data.key) return failure(502, 'The gateway did not return a signed-in account.');
    if (this.owner && this.owner !== data.user_id) {
      if (token) this.flows.delete(token);
      return failure(403, 'This account is not paired with this Mac.');
    }
    if (!this.owner) {
      if (!pending.canPair) return failure(403, 'Pair this Mac from localhost first.');
      this.owner = data.user_id;
      try {
        await mkdir(dirname(this.ownerFile), { recursive: true, mode: 0o700 });
        await writeFile(this.ownerFile, JSON.stringify({ subject: data.user_id }), { mode: 0o600 });
      } catch {
        this.owner = null;
        return failure(500, 'Could not save this device’s account pairing.');
      }
    }
    if (token) this.flows.delete(token);
    return { ok: true, value: { status: 'ready', ...this.session(data.user_id, 'sso') } };
  }

  async local(local: boolean): Promise<Result<{ token: string; identity: Identity }>> {
    if (!local || !this.gatewayKey) return failure(403, 'This sign-in is only available on the Mac itself.');
    if (!await this.gateway.verify(this.gatewayKey)) return failure(401, 'The gateway connection on this Mac is no longer valid.');
    return { ok: true, value: this.session('local-gateway', 'gateway') };
  }

}
