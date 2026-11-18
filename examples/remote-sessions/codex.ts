import WebSocket from 'ws';
import { z } from 'zod';
import { randomUUID } from 'node:crypto';
import { stat } from 'node:fs/promises';
import { isAbsolute } from 'node:path';
import { failure, publicError, redact } from './protocol.ts';
import { sessionSummary, threadPageSchema, threadSchema, turnPageSchema, visibleMessage } from './codex-protocol.ts';
import type { Thread } from './codex-protocol.ts';
import type { Approval, Message, ModelPage, Result, SessionDetail, SessionSummary, Sessions } from './protocol.ts';

const replySchema = z.object({
  id: z.number().optional(),
  result: z.unknown().optional(),
  error: z.object({ code: z.number(), message: z.string() }).optional(),
});
const serverMessageSchema = z.object({
  id: z.union([z.number(), z.string()]).optional(), method: z.string(), params: z.unknown(),
});
const approvalSchema = z.object({
  threadId: z.string(), turnId: z.string(), itemId: z.string(), reason: z.string().nullish(), command: z.string().nullish(),
  grantRoot: z.string().nullish(), networkApprovalContext: z.object({ host: z.string(), protocol: z.string() }).nullish(),
  additionalPermissions: z.unknown().optional(), availableDecisions: z.array(z.unknown()).nullish(),
});
type PendingApproval = Readonly<{ requestId: number | string; threadId: string; turnId: string; approval: Approval }>;
type LiveMessage = Readonly<{ threadId: string; turnId: string; itemId: string; text: string; complete: boolean; updatedAt: number }>;

export class CodexSessions implements Sessions {
  private socket: WebSocket | null = null;
  private connecting: Promise<Result<true>> | null = null;
  private nextId = 0;
  private readonly pending = new Map<number, (value: unknown) => void>();
  private readonly approvals = new Map<string, PendingApproval>();
  private readonly changes = new Map<string, string>();
  private readonly drafts = new Map<string, Thread>();
  private readonly startingTurns = new Map<string, Readonly<{ text: string; startedAt: number }>>();
  private readonly tracked = new Set<string>();
  private readonly liveMessages = new Map<string, LiveMessage>();
  readonly socketPath: string;
  readonly provider: string;
  readonly gatewayUrl: string;

  constructor(socketPath: string, provider: string, gatewayUrl: string) {
    this.socketPath = socketPath;
    this.provider = provider;
    this.gatewayUrl = gatewayUrl;
  }

  private async connect(): Promise<Result<true>> {
    if (this.socket?.readyState === WebSocket.OPEN && !this.connecting) return { ok: true, value: true };
    if (this.connecting) return this.connecting;
    this.connecting = this.open();
    const result = await this.connecting;
    this.connecting = null;
    return result;
  }

  private async open(): Promise<Result<true>> {
    const socket = new WebSocket(`ws+unix://${this.socketPath}:/`, { handshakeTimeout: 5000, maxPayload: 32 * 1024 * 1024 });
    this.socket = socket;
    socket.on('message', data => {
      try {
        const raw: unknown = JSON.parse(data.toString());
        const event = serverMessageSchema.safeParse(raw);
        if (event.success) { this.event(event.data); return; }
        const parsed = replySchema.safeParse(raw);
        if (!parsed.success || parsed.data.id === undefined) return;
        this.pending.get(parsed.data.id)?.(parsed.data);
        this.pending.delete(parsed.data.id);
      } catch {}
    });
    socket.on('error', () => {});
    socket.on('close', () => {
      this.approvals.clear();
      this.changes.clear();
      this.drafts.clear();
      this.startingTurns.clear();
      this.tracked.clear();
      this.liveMessages.clear();
      for (const resolve of this.pending.values()) resolve({ error: { code: -1, message: 'Codex disconnected' } });
      this.pending.clear();
    });
    const opened = await new Promise<boolean>(resolve => {
      socket.once('open', () => resolve(true));
      socket.once('error', () => resolve(false));
    });
    if (!opened) return failure(503, 'Cannot reach Codex. Keep Codex running on this Mac.');
    const initialized = await this.request('initialize', {
      clientInfo: { name: 'litellm_remote_control', title: 'LiteLLM Remote', version: '0.1.0' },
      capabilities: { experimentalApi: true },
    }, z.object({ userAgent: z.string() }));
    if (!initialized.ok) {
      socket.close();
      return initialized;
    }
    socket.send(JSON.stringify({ method: 'initialized' }));
    return { ok: true, value: true };
  }

  private async request<T>(method: string, params: Readonly<Record<string, unknown>>, schema: z.ZodType<T>): Promise<Result<T>> {
    const socket = this.socket;
    if (!socket || socket.readyState !== WebSocket.OPEN) return failure(503, 'Codex is disconnected.');
    this.nextId += 1;
    const id = this.nextId;
    const raw = await new Promise<unknown>(resolve => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        resolve({ error: { code: -1, message: 'Codex did not answer in time' } });
      }, 10000);
      this.pending.set(id, value => { clearTimeout(timer); resolve(value); });
      socket.send(JSON.stringify({ id, method, params }));
    });
    const reply = replySchema.safeParse(raw);
    if (!reply.success) return failure(502, 'Codex returned an unreadable response.');
    if (reply.data.error) return failure(502, publicError(reply.data.error.message));
    const parsed = schema.safeParse(reply.data.result);
    return parsed.success ? { ok: true, value: parsed.data } : failure(502, 'This Codex response is not supported yet.');
  }

  private event(event: z.infer<typeof serverMessageSchema>): void {
    if (event.method === 'item/agentMessage/delta') {
      const delta = z.object({ threadId: z.string(), turnId: z.string(), itemId: z.string(), delta: z.string() }).safeParse(event.params);
      if (delta.success && this.tracked.has(delta.data.threadId)) {
        const value = delta.data;
        const key = `${value.threadId}/${value.turnId}/${value.itemId}`;
        const previous = this.liveMessages.get(key);
        this.liveMessages.set(key, { threadId: value.threadId, turnId: value.turnId, itemId: value.itemId,
          text: ((previous?.text ?? '') + value.delta).slice(0, 256000), complete: false, updatedAt: Date.now() });
        const first = this.liveMessages.keys().next().value;
        if (this.liveMessages.size > 128 && first) this.liveMessages.delete(first);
      }
      return;
    }
    if (event.method === 'item/started') {
      const change = z.object({ threadId: z.string(), item: z.object({ id: z.string(), type: z.literal('fileChange'),
        changes: z.array(z.object({ path: z.string(), diff: z.string() })).min(1),
      }) }).safeParse(event.params);
      if (change.success) {
        const text = change.data.item.changes.map(value => `${value.path}\n${value.diff}`).join('\n\n');
        if (text.length <= 32000) this.changes.set(`${change.data.threadId}/${change.data.item.id}`, text);
        const first = this.changes.keys().next().value;
        if (this.changes.size > 100 && first) this.changes.delete(first);
      }
      return;
    }
    if (event.method === 'item/completed') {
      const completed = z.object({ threadId: z.string(), item: z.object({ id: z.string() }) }).safeParse(event.params);
      if (completed.success) this.changes.delete(`${completed.data.threadId}/${completed.data.item.id}`);
      const message = z.object({ threadId: z.string(), turnId: z.string(),
        item: z.object({ id: z.string(), type: z.literal('agentMessage'), text: z.string() }),
      }).safeParse(event.params);
      if (message.success && this.tracked.has(message.data.threadId)) {
        const value = message.data;
        this.liveMessages.set(`${value.threadId}/${value.turnId}/${value.item.id}`, { threadId: value.threadId,
          turnId: value.turnId, itemId: value.item.id, text: value.item.text.slice(0, 256000), complete: true, updatedAt: Date.now() });
      }
      return;
    }
    if (event.method === 'serverRequest/resolved') {
      const resolved = z.object({ requestId: z.union([z.number(), z.string()]) }).safeParse(event.params);
      if (resolved.success) for (const [key, pending] of this.approvals) {
        if (pending.requestId === resolved.data.requestId) this.approvals.delete(key);
      }
      return;
    }
    if (event.method === 'turn/completed') {
      const completed = z.object({ threadId: z.string(), turn: z.object({ id: z.string() }) }).safeParse(event.params);
      if (completed.success) for (const [key, pending] of this.approvals) {
        if (pending.threadId === completed.data.threadId && pending.turnId === completed.data.turn.id) this.approvals.delete(key);
      }
      return;
    }
    if (event.id === undefined) return;
    if (!['item/commandExecution/requestApproval', 'item/fileChange/requestApproval'].includes(event.method)) {
      this.socket?.send(JSON.stringify({ id: event.id, error: { code: -32601, message: 'This request needs a supported Codex client.' } }));
      return;
    }
    const parsed = approvalSchema.safeParse(event.params);
    if (!parsed.success) {
      this.socket?.send(JSON.stringify({ id: event.id, result: { decision: 'decline' } }));
      return;
    }
    const params = parsed.data;
    const key = randomUUID();
    const network = params.networkApprovalContext;
    const isCommand = event.method === 'item/commandExecution/requestApproval';
    const changes = this.changes.get(`${params.threadId}/${params.itemId}`);
    const title = network ? 'Allow network access?' : isCommand ? 'Run this command?' : 'Allow file changes?';
    const canReview = isCommand ? Boolean(params.command || network || params.additionalPermissions) : Boolean(changes && !params.grantRoot);
    const text = [network ? `${network.protocol}://${network.host}` : params.command || params.grantRoot, params.reason,
      !isCommand ? changes : null, !canReview ? 'Open this session on your Mac to review the complete request.' : null,
      params.additionalPermissions ? JSON.stringify(params.additionalPermissions) : null].filter(Boolean).join('\n\n');
    this.approvals.set(key, { requestId: event.id, threadId: params.threadId, turnId: params.turnId,
      approval: { id: key, title, text: redact(text || 'Codex is requesting approval.'),
        canAccept: canReview && (!params.availableDecisions || params.availableDecisions.includes('accept')) } });
  }

  private async metadata(id: string) {
    if (!z.string().uuid().safeParse(id).success) return failure(404, 'Session not found.');
    const connected = await this.connect();
    if (!connected.ok) return connected;
    const draft = this.drafts.get(id);
    if (draft) return { ok: true as const, value: draft };
    const metadata = await this.request('thread/read', { threadId: id }, z.object({ thread: threadSchema }));
    if (!metadata.ok) return metadata;
    return metadata.value.thread.modelProvider === this.provider
      ? { ok: true as const, value: metadata.value.thread } : failure(404, 'Session not found.');
  }

  private async verifyGateway(cwd: string): Promise<Result<true>> {
    const configuration = await this.request('config/read', { cwd }, z.object({ config: z.object({
      model_providers: z.record(z.string(), z.object({ base_url: z.string().optional() })).optional(),
    }) }));
    if (!configuration.ok) return configuration;
    const endpoint = configuration.value.config.model_providers?.[this.provider]?.base_url;
    const normalize = (value: string) => value.replace(/\/+$/, '').replace(/\/v1$/, '');
    if (endpoint && normalize(endpoint) === normalize(this.gatewayUrl)) return { ok: true, value: true };
    const local = endpoint && URL.canParse(endpoint) ? new URL(endpoint) : null;
    if (local?.protocol === 'http:' && ['127.0.0.1', 'localhost', '[::1]'].includes(local.hostname)
      && local.pathname.replace(/\/$/, '') === '/v1' && !local.username && !local.password && !local.search && !local.hash) {
      try {
        const response = await fetch(new URL('/api/status', local), { redirect: 'error', signal: AbortSignal.timeout(3000) });
        const relay = z.object({ runtime: z.literal('rust'), gateway_url: z.string(),
          account: z.object({ gateway: z.object({ url: z.string() }) }).nullish(),
        }).safeParse(await response.json());
        if (response.ok && relay.success && normalize(relay.data.account?.gateway.url ?? relay.data.gateway_url) === normalize(this.gatewayUrl)) return { ok: true, value: true };
      } catch {}
    }
    return failure(409, 'This project’s Codex provider must point to the connected LiteLLM gateway.');
  }

  async create(cwd: string): Promise<Result<SessionSummary>> {
    if (!isAbsolute(cwd) || !(await stat(cwd).catch(() => null))?.isDirectory()) return failure(400, 'Choose an existing project folder on your Mac.');
    const connected = await this.connect();
    if (!connected.ok) return connected;
    const verified = await this.verifyGateway(cwd);
    if (!verified.ok) return verified;
    const started = await this.request('thread/start', {
      modelProvider: this.provider, cwd, approvalPolicy: 'on-request', sandbox: 'workspace-write',
      serviceName: 'litellm_remote',
    }, z.object({ thread: threadSchema }));
    if (!started.ok) return started;
    if (started.value.thread.modelProvider !== this.provider) return failure(502, 'Codex did not select the LiteLLM gateway.');
    this.drafts.set(started.value.thread.id, started.value.thread);
    this.tracked.add(started.value.thread.id);
    return { ok: true, value: sessionSummary(started.value.thread, true) };
  }

  async models(cursor?: string): Promise<Result<ModelPage>> {
    const connected = await this.connect();
    if (!connected.ok) return connected;
    const page = await this.request('model/list', { modelProvider: this.provider, limit: 100, ...(cursor ? { cursor } : {}) },
      z.object({ data: z.array(z.object({ model: z.string(), displayName: z.string(), isDefault: z.boolean(), hidden: z.boolean().default(false) })), nextCursor: z.string().nullish() }));
    if (!page.ok) return page;
    return { ok: true, value: { models: page.value.data.filter(model => !model.hidden)
      .map(model => ({ id: model.model, label: model.displayName || model.model, isDefault: model.isDefault })), nextCursor: page.value.nextCursor ?? null } };
  }

  async send(id: string, text: string, model?: string): ReturnType<Sessions['send']> {
    if (!text.trim() || text.length > 32000) return failure(400, 'Enter a message up to 32,000 characters.');
    const metadata = await this.metadata(id);
    if (!metadata.ok) return metadata;
    const verified = await this.verifyGateway(metadata.value.cwd);
    if (!verified.ok) return verified;
    if (metadata.value.canAcceptDirectInput === false) return failure(409, 'This session does not accept direct messages. Open its parent session.');
    if (model && metadata.value.status.type === 'active' && metadata.value.model !== model) return failure(409, 'Wait for the current reply before switching models.');
    this.tracked.add(id);
    if (!this.drafts.has(id)) {
      const resumed = await this.request('thread/resume', { threadId: id, modelProvider: this.provider, excludeTurns: true }, z.object({ thread: threadSchema }));
      if (!resumed.ok) return resumed;
      if (resumed.value.thread.modelProvider !== this.provider) return failure(409, 'This session is no longer using the LiteLLM gateway.');
    }
    const started = await this.request('turn/start', { threadId: id, input: [{ type: 'text', text, text_elements: [] }], ...(model ? { model } : {}) },
      z.object({ turn: z.object({ id: z.string() }) }));
    if (!started.ok) return started;
    const draft = this.drafts.get(id);
    if (draft) {
      this.drafts.set(id, { ...draft, model: model ?? draft.model, status: { type: 'active' } });
      this.startingTurns.set(id, { text, startedAt: Date.now() });
    }
    return { ok: true, value: { turnId: started.value.turn.id } };
  }

  async approve(id: string, approvalId: string, decision: 'accept' | 'decline'): ReturnType<Sessions['approve']> {
    const metadata = await this.metadata(id);
    if (!metadata.ok) return metadata;
    const pending = this.approvals.get(approvalId);
    if (!pending || pending.threadId !== id) return failure(409, 'This approval was already resolved.');
    if (decision === 'accept' && !pending.approval.canAccept) return failure(400, 'Review this request on your Mac.');
    if (this.socket?.readyState !== WebSocket.OPEN) return failure(503, 'Codex is disconnected.');
    this.socket.send(JSON.stringify({ id: pending.requestId, result: { decision } }));
    this.approvals.delete(approvalId);
    return { ok: true, value: { resolved: true } };
  }

  async list(cursor?: string): ReturnType<Sessions['list']> {
    const connected = await this.connect();
    if (!connected.ok) return connected;
    const page = await this.request('thread/list', {
      limit: 50, sortKey: 'updated_at', modelProviders: [this.provider],
      sourceKinds: ['cli', 'vscode', 'appServer'], ...(cursor ? { cursor } : {}),
    }, threadPageSchema);
    if (!page.ok) return page;
    return {
      ok: true,
      value: {
        sessions: [...page.value.data, ...(!cursor ? [...this.drafts.values()].filter(draft => !page.value.data.some(thread => thread.id === draft.id)) : [])]
          .map(thread => sessionSummary(thread, ['active', 'idle'].includes(thread.status.type))),
        nextCursor: page.value.nextCursor ?? null,
      },
    };
  }

  async read(id: string): Promise<Result<SessionDetail>> {
    const metadata = await this.metadata(id);
    if (!metadata.ok) return metadata;
    if (this.drafts.has(id) && !this.startingTurns.has(id)) return { ok: true, value: {
      session: sessionSummary(metadata.value, true), messages: [], olderMessages: false, approvals: [],
    } };
    const turns = await this.request('thread/turns/list', {
      threadId: id, limit: 12, sortDirection: 'desc', itemsView: 'full',
    }, turnPageSchema);
    if (!turns.ok) {
      const starting = this.startingTurns.get(id);
      if (starting && Date.now() - starting.startedAt < 30000) return { ok: true, value: {
        session: sessionSummary({ ...metadata.value, status: { type: 'active' } }, true),
        messages: this.withLiveMessages(id, [{ id: `${id}/pending`, role: 'user', text: redact(starting.text) }]), olderMessages: false,
        approvals: [...this.approvals.values()].filter(value => value.threadId === id).map(value => value.approval),
      } };
      return turns;
    }
    this.drafts.delete(id);
    this.startingTurns.delete(id);
    const thread = metadata.value;
    return {
      ok: true,
      value: {
        session: sessionSummary(thread, ['active', 'idle'].includes(thread.status.type)),
        messages: this.withLiveMessages(id, turns.value.data.toReversed().flatMap(turn => turn.items.flatMap(item => visibleMessage(item, turn.id)))),
        olderMessages: Boolean(turns.value.nextCursor),
        error: turns.value.data[0]?.error?.message ? publicError(turns.value.data[0].error.message) : undefined,
        approvals: [...this.approvals.values()].filter(value => value.threadId === id).map(value => value.approval),
      },
    };
  }

  private withLiveMessages(threadId: string, stored: readonly Message[]): readonly Message[] {
    for (const [key, message] of this.liveMessages) {
      const persisted = stored.find(value => value.id === `${message.turnId}/${message.itemId}`);
      if (Date.now() - message.updatedAt > 300000 || (message.threadId === threadId && message.complete && persisted?.text.startsWith(redact(message.text)))) this.liveMessages.delete(key);
    }
    const live = [...this.liveMessages.values()].filter(value => value.threadId === threadId)
      .map(value => ({ id: `${value.turnId}/${value.itemId}`, role: 'assistant' as const, text: redact(value.text) }));
    const merged = stored.map(message => {
      const newer = live.find(value => value.id === message.id);
      return newer && newer.text.length > message.text.length ? newer : message;
    });
    return [...merged, ...live.filter(message => !stored.some(value => value.id === message.id))];
  }

  async interrupt(id: string): ReturnType<Sessions['interrupt']> {
    const metadata = await this.metadata(id);
    if (!metadata.ok) return metadata;
    const verified = await this.verifyGateway(metadata.value.cwd);
    if (!verified.ok) return verified;
    const turns = await this.request('thread/turns/list', { threadId: id, limit: 1, sortDirection: 'desc' }, turnPageSchema);
    if (!turns.ok) return turns;
    const active = turns.value.data.find(turn => turn.status === 'inProgress');
    if (!active) return { ok: true, value: { stopped: false } };
    const interrupted = await this.request('turn/interrupt', { threadId: id, turnId: active.id }, z.object({}));
    return interrupted.ok ? { ok: true, value: { stopped: true } } : interrupted;
  }

  close(): void { this.socket?.close(); }
}
