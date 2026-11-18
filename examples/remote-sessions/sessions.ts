import { z } from 'zod';
import { failure, sessionIdSchema } from './protocol.ts';
import type { AgentKind, Result, SessionSummary, Sessions } from './protocol.ts';

const cursorSchema = z.object({ codex: z.string().nullable(), claude: z.string().nullable() }).strict();

export class RemoteSessions implements Sessions {
  private readonly agents: Readonly<Record<AgentKind, Sessions>>;

  constructor(agents: Readonly<Record<AgentKind, Sessions>>) { this.agents = agents; }

  private route(id: string): Result<Readonly<{ sessions: Sessions; id: string; agent: AgentKind }>> {
    if (!sessionIdSchema.safeParse(id).success) return failure(404, 'Session not found.');
    const [agent, nativeId] = id.split(':');
    if (agent !== 'codex' && agent !== 'claude') return failure(404, 'Session not found.');
    return { ok: true, value: { sessions: this.agents[agent], id: nativeId, agent } };
  }

  private publicSession(agent: AgentKind, session: SessionSummary): SessionSummary {
    return { ...session, id: `${agent}:${session.id}`, agent };
  }

  async list(cursor?: string): ReturnType<Sessions['list']> {
    const decoded = (() => {
      if (!cursor) return undefined;
      try { return cursorSchema.safeParse(JSON.parse(Buffer.from(cursor, 'base64url').toString())); }
      catch { return null; }
    })();
    if (cursor && !decoded?.success) return failure(400, 'Refresh the session list.');
    const cursors = decoded?.success ? decoded.data : undefined;
    const load = (agent: AgentKind) => cursors?.[agent] === null
      ? Promise.resolve({ ok: true as const, value: { sessions: [], nextCursor: null } })
      : this.agents[agent].list(cursors?.[agent] ?? undefined);
    const [codex, claude] = await Promise.all([load('codex'), load('claude')]);
    if (!codex.ok && !claude.ok) return failure(503, 'Neither coding agent is available on this Mac.');
    const sessions = [
      ...(codex.ok ? codex.value.sessions.map(session => this.publicSession('codex', session)) : []),
      ...(claude.ok ? claude.value.sessions.map(session => this.publicSession('claude', session)) : []),
    ].toSorted((a, b) => b.updatedAt - a.updatedAt);
    const next = { codex: codex.ok ? codex.value.nextCursor : null, claude: claude.ok ? claude.value.nextCursor : null };
    return { ok: true, value: { sessions, nextCursor: next.codex || next.claude ? Buffer.from(JSON.stringify(next)).toString('base64url') : null,
      warnings: [codex.ok ? '' : codex.error, claude.ok ? '' : claude.error].filter(Boolean) } };
  }

  async read(id: string): ReturnType<Sessions['read']> {
    const route = this.route(id);
    if (!route.ok) return route;
    const detail = await route.value.sessions.read(route.value.id);
    return detail.ok ? { ok: true, value: { ...detail.value, session: this.publicSession(route.value.agent, detail.value.session) } } : detail;
  }

  async create(cwd: string, agent: AgentKind = 'codex'): ReturnType<Sessions['create']> {
    const created = await this.agents[agent].create(cwd);
    return created.ok ? { ok: true, value: this.publicSession(agent, created.value) } : created;
  }

  async models(): ReturnType<Sessions['models']> {
    const results = await Promise.all([this.agents.codex.models(), this.agents.claude.models()]);
    return { ok: true, value: { models: results.flatMap(result => result.ok ? result.value.models : []), nextCursor: null } };
  }

  async send(id: string, text: string, model?: string): ReturnType<Sessions['send']> {
    const route = this.route(id);
    return route.ok ? route.value.sessions.send(route.value.id, text, model) : route;
  }

  async approve(id: string, approvalId: string, decision: 'accept' | 'decline'): ReturnType<Sessions['approve']> {
    const route = this.route(id);
    return route.ok ? route.value.sessions.approve(route.value.id, approvalId, decision) : route;
  }

  async interrupt(id: string): ReturnType<Sessions['interrupt']> {
    const route = this.route(id);
    return route.ok ? route.value.sessions.interrupt(route.value.id) : route;
  }
}
