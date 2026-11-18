import { randomUUID } from 'node:crypto';
import { mkdir, readFile, readdir, rename, stat, unlink, writeFile } from 'node:fs/promises';
import { basename, isAbsolute, join } from 'node:path';
import { z } from 'zod';
import { failure, redact } from './protocol.ts';
import type { Approval, Message, Result, SessionSummary, Sessions } from './protocol.ts';
import type { ClaudeEvent, ClaudeRun, ClaudeRuntime } from './claude-runtime.ts';

const grantSchema = z.object({ id: z.string().uuid(), cwd: z.string().refine(isAbsolute), title: z.string(),
  model: z.string(), updatedAt: z.number() }).strict().readonly();
type Grant = z.infer<typeof grantSchema>;
type Running = Readonly<{ generation: string; opening: Promise<Result<ClaudeRun>>; working: boolean; stopping: boolean;
  model: string; closed?: boolean; error?: string; messages: readonly Message[]; updatedAt: number }>;
type Decision = Readonly<{ sessionId: string; approval: Approval; resolve: (accepted: boolean) => void }>;

export class ClaudeSessions implements Sessions {
  private readonly runtime: ClaudeRuntime;
  private readonly directory: string;
  private readonly defaultModel: string;
  private readonly runs = new Map<string, Running>();
  private stopped = false;
  private readonly sending = new Set<string>();
  private readonly decisions = new Map<string, Decision>();

  constructor(runtime: ClaudeRuntime, directory: string, defaultModel = 'opus') {
    this.runtime = runtime;
    this.directory = directory;
    this.defaultModel = defaultModel;
  }

  private async grants(): Promise<Result<readonly Grant[]>> {
    try {
      const files = await readdir(this.directory).catch(error => z.object({ code: z.literal('ENOENT') }).safeParse(error).success ? [] : null);
      if (!files) return failure(503, 'Claude handoff records could not be read. Check Relay on this Mac.');
      const values = await Promise.all(files.filter(file => /^[a-f0-9-]{36}\.json$/.test(file))
        .map(async file => grantSchema.parse(JSON.parse(await readFile(join(this.directory, file), 'utf8')))));
      return { ok: true, value: values };
    } catch { return failure(503, 'Claude handoff records could not be read. Check Relay on this Mac.'); }
  }

  private async grant(id: string): Promise<Result<Grant>> {
    if (!z.string().uuid().safeParse(id).success) return failure(404, 'Session not found.');
    const grants = await this.grants();
    if (!grants.ok) return grants;
    const value = grants.value.find(grant => grant.id === id);
    return value ? { ok: true, value } : failure(409, 'Finish the current turn, exit Claude, then hand this session over from Relay on your Mac.');
  }

  private async save(grant: Grant): Promise<Result<Grant>> {
    try {
      await mkdir(this.directory, { recursive: true, mode: 0o700 });
      await writeFile(join(this.directory, `${grant.id}.json`), JSON.stringify(grant), { flag: 'wx', mode: 0o600 });
      return { ok: true, value: grant };
    } catch (error) {
      return z.object({ code: z.literal('EEXIST') }).safeParse(error).success ? this.grant(grant.id)
        : failure(503, 'Relay could not save the Claude handoff.');
    }
  }

  private async remember(grant: Grant): Promise<boolean> {
    const pending = join(this.directory, `${grant.id}.${randomUUID()}.pending`);
    try {
      await writeFile(pending, JSON.stringify(grant), { flag: 'wx', mode: 0o600 });
      await rename(pending, join(this.directory, `${grant.id}.json`));
      return true;
    } catch { return false; }
    finally { await unlink(pending).catch(() => {}); }
  }

  async handoff(id: string): Promise<Result<SessionSummary>> {
    if (!z.string().uuid().safeParse(id).success) return failure(400, 'Use the Claude session UUID shown by /status.');
    const active = await this.runtime.active();
    if (!active.ok) return active;
    if (active.value.some(session => session.id === id)) return failure(409, 'This session is still open in Claude. Finish its current turn and exit it before handing it over.');
    const info = await this.runtime.info(id);
    if (!info.ok) return info;
    const saved = await this.save({ ...info.value, model: this.defaultModel });
    return saved.ok ? { ok: true, value: this.summary(saved.value) } : saved;
  }

  private owns(id: string): boolean {
    const run = this.runs.get(id);
    return Boolean(run && !run.closed);
  }

  private summary(grant: Grant, external = false): SessionSummary {
    const run = this.runs.get(grant.id);
    const approvals = [...this.decisions.values()].some(value => value.sessionId === grant.id);
    return { id: grant.id, agent: 'claude', title: redact(grant.title).slice(0, 140), project: basename(grant.cwd), cwd: grant.cwd,
      model: run?.model ?? grant.model, updatedAt: run?.updatedAt ?? grant.updatedAt,
      status: external ? 'handoff_required' : approvals ? 'needs_approval' : run?.working ? 'working' : run?.error ? 'error' : 'ready',
      canSend: !external && !run?.working, canStop: !external && Boolean(run?.working || approvals) };
  }

  async list(cursor?: string): ReturnType<Sessions['list']> {
    if (cursor) return failure(400, 'Refresh the Claude session list.');
    const grants = await this.grants();
    if (!grants.ok) return grants;
    const active = await this.runtime.active();
    if (!active.ok) return active;
    const known = grants.value.map(grant => this.summary(grant, !this.owns(grant.id) && active.value.some(value => value.id === grant.id)));
    const discovered = active.value.filter(value => !grants.value.some(grant => grant.id === value.id))
      .map(value => this.summary({ ...value, model: this.defaultModel }, true));
    return { ok: true, value: { sessions: [...known, ...discovered].toSorted((a, b) => b.updatedAt - a.updatedAt), nextCursor: null } };
  }

  async read(id: string): ReturnType<Sessions['read']> {
    const grant = await this.grant(id);
    const active = this.owns(id) ? { ok: true as const, value: [] } : await this.runtime.active();
    if (!active.ok) return active;
    const external = active.value.find(value => value.id === id);
    if (!grant.ok) {
      if (!external) return grant;
      return { ok: true, value: { session: this.summary({ ...external, model: this.defaultModel }, true), messages: [], olderMessages: false,
        approvals: [], error: 'On your Mac, finish the current turn and note the session UUID in /status. Run /exit, then npm run agent -- --handoff ' + id } };
    }
    if (external) return { ok: true, value: { session: this.summary(grant.value, true), messages: [], olderMessages: false,
      approvals: [], error: 'This session is open outside Relay. Exit it on your Mac before continuing here.' } };
    const history = await this.runtime.history(id, grant.value.cwd);
    if (!history.ok) return history;
    const run = this.runs.get(id);
    const live = run?.messages ?? [];
    const merged = history.value.map(message => live.find(value => value.id === message.id) ?? message);
    const messages = [...merged, ...live.filter(message => !history.value.some(value => value.id === message.id))];
    return { ok: true, value: { session: this.summary(grant.value), messages: messages.slice(-120), olderMessages: messages.length > 120,
      approvals: [...this.decisions.values()].filter(value => value.sessionId === id).map(value => value.approval), error: run?.error } };
  }

  async create(cwd: string): ReturnType<Sessions['create']> {
    if (!isAbsolute(cwd) || !(await stat(cwd).catch(() => null))?.isDirectory()) return failure(400, 'Choose an existing project folder on your Mac.');
    const grant = await this.save({ id: randomUUID(), cwd, title: 'New Claude session', model: this.defaultModel, updatedAt: Math.floor(Date.now() / 1000) });
    return grant.ok ? { ok: true, value: this.summary(grant.value) } : grant;
  }

  async models(): ReturnType<Sessions['models']> { return { ok: true, value: { models: [], nextCursor: null } }; }

  private event(id: string, generation: string, event: ClaudeEvent): void {
    const run = this.runs.get(id);
    if (!run || run.generation !== generation) return;
    if (event.type === 'init') {
      if (event.id !== id) {
        void run.opening.then(opened => { if (opened.ok) opened.value.close(); });
        this.runs.set(id, { ...run, working: false, error: 'Claude opened a different session. Relay stopped it to avoid sending into a copy.' });
        return;
      }
      this.runs.set(id, { ...run, model: event.model });
      return;
    }
    if (event.type === 'message') {
      const previous = run.messages.find(message => message.id === event.message.id);
      const message = { ...event.message, text: redact((event.append ? previous?.text ?? '' : '') + event.message.text).slice(0, 32000) };
      this.runs.set(id, { ...run, messages: [...run.messages.filter(value => value.id !== message.id), message].slice(-120), updatedAt: Math.floor(Date.now() / 1000) });
      return;
    }
    for (const [key, pending] of this.decisions) if (pending.sessionId === id) { pending.resolve(false); this.decisions.delete(key); }
    this.runs.set(id, { ...run, working: false, stopping: false, error: run.stopping ? undefined : event.error, updatedAt: Math.floor(Date.now() / 1000) });
    if (event.type === 'closed') this.runs.set(id, { ...run, closed: true, working: false, stopping: false, error: run.stopping ? undefined : event.error ?? run.error });
  }

  private approveTool(id: string, tool: string, input: Readonly<Record<string, unknown>>, signal: AbortSignal): Promise<boolean> {
    if (signal.aborted) return Promise.resolve(false);
    const key = randomUUID();
    const full = `${tool}\n${JSON.stringify(input, null, 2)}`;
    return new Promise(resolve => {
      const cancel = () => settle(false);
      const timer = setTimeout(cancel, 10 * 60 * 1000);
      timer.unref();
      const settle = (accepted: boolean) => { clearTimeout(timer); signal.removeEventListener('abort', cancel); this.decisions.delete(key); resolve(accepted); };
      this.decisions.set(key, { sessionId: id, resolve: settle, approval: { id: key, title: `Allow ${tool}?`,
        text: redact(full).slice(0, 32000), canAccept: full.length <= 32000 && !['AskUserQuestion', 'ExitPlanMode'].includes(tool) } });
      signal.addEventListener('abort', cancel, { once: true });
    });
  }

  private async start(grant: Grant): Promise<Result<ClaudeRun>> {
    if (this.stopped) return failure(503, 'This connector is shutting down.');
    const existing = this.runs.get(grant.id);
    if (existing && !existing.closed) return existing.opening;
    const active = await this.runtime.active();
    if (!active.ok) return active;
    if (active.value.some(session => session.id === grant.id)) return failure(409, 'This session is open outside Relay. Exit it on your Mac before continuing here.');
    if (this.stopped) return failure(503, 'This connector is shutting down.');
    const concurrent = this.runs.get(grant.id);
    if (concurrent && !concurrent.closed) return concurrent.opening;
    if (this.runs.size >= 4) {
      const idle = [...this.runs].find(([, run]) => !run.working);
      if (!idle) return failure(429, 'Four Claude sessions are already working. Wait for one to finish.');
      void idle[1].opening.then(opened => { if (opened.ok) opened.value.close(); });
      this.runs.delete(idle[0]);
    }
    const generation = randomUUID();
    const opening = (async () => {
      const info = await this.runtime.info(grant.id);
      if (!info.ok && info.status !== 404) return info;
      return this.runtime.open({ id: grant.id, cwd: grant.cwd, model: grant.model, resume: info.ok,
        event: event => this.event(grant.id, generation, event), approve: (tool, input, signal) => this.approveTool(grant.id, tool, input, signal) });
    })();
    this.runs.set(grant.id, { generation, opening, working: true, stopping: false, model: grant.model, messages: [], updatedAt: Math.floor(Date.now() / 1000) });
    const opened = await opening;
    if (!opened.ok) this.runs.delete(grant.id);
    return opened;
  }

  async send(id: string, text: string, model?: string): ReturnType<Sessions['send']> {
    if (this.sending.has(id)) return failure(409, 'A message is already being sent to this session.');
    this.sending.add(id);
    try { return await this.sendMessage(id, text, model); }
    finally { this.sending.delete(id); }
  }

  private async sendMessage(id: string, text: string, model?: string): ReturnType<Sessions['send']> {
    if (!text.trim() || text.length > 32000) return failure(400, 'Enter a message up to 32,000 characters.');
    const grant = await this.grant(id);
    if (!grant.ok) return grant;
    if (this.runs.get(id)?.working) return failure(409, 'Wait for Claude to finish or stop its current turn before sending another message.');
    const opened = await this.start(grant.value);
    if (!opened.ok) return opened;
    const run = this.runs.get(id);
    if (!run) return failure(503, 'Claude disconnected.');
    this.runs.set(id, { ...run, working: true, stopping: false, error: undefined, updatedAt: Math.floor(Date.now() / 1000) });
    const sent = await opened.value.send(text, model);
    const current = this.runs.get(id);
    if (current) this.runs.set(id, sent.ok ? { ...current, messages: [...current.messages,
      { id: sent.value.turnId, role: 'user' as const, text: redact(text) }].slice(-120), model: model ?? current.model }
      : { ...current, working: false, error: sent.error });
    if (sent.ok) {
      const remembered = await this.remember({ ...grant.value, model: model ?? current?.model ?? grant.value.model,
        title: grant.value.title === 'New Claude session' ? redact(text.split('\n')[0]).slice(0, 140) : grant.value.title,
        updatedAt: Math.floor(Date.now() / 1000) });
      if (!remembered) console.error('A Claude message was accepted, but its local session metadata could not be saved.');
    }
    return sent;
  }

  async approve(id: string, approvalId: string, decision: 'accept' | 'decline'): ReturnType<Sessions['approve']> {
    const pending = this.decisions.get(approvalId);
    if (!pending || pending.sessionId !== id) return failure(409, 'This approval is no longer pending.');
    if (decision === 'accept' && !pending.approval.canAccept) return failure(400, 'This request cannot be approved from the phone. Stop the turn and continue on your Mac.');
    pending.resolve(decision === 'accept');
    return { ok: true, value: { resolved: true } };
  }

  async interrupt(id: string): ReturnType<Sessions['interrupt']> {
    const grant = await this.grant(id);
    if (!grant.ok) return grant;
    const run = this.runs.get(id);
    if (!run?.working) return { ok: true, value: { stopped: false } };
    this.runs.set(id, { ...run, stopping: true });
    const opened = await run.opening;
    if (!opened.ok) return opened;
    return opened.value.interrupt();
  }

  close(): void {
    this.stopped = true;
    for (const pending of this.decisions.values()) pending.resolve(false);
    this.decisions.clear();
    for (const run of this.runs.values()) void run.opening.then(opened => { if (opened.ok) opened.value.close(); });
    this.runs.clear();
  }
}
