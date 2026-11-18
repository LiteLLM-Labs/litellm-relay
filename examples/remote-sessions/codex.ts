import WebSocket from 'ws';
import { z } from 'zod';
import { failure, sessionSummary, threadPageSchema, threadSchema, turnPageSchema, visibleMessage } from './protocol.ts';
import type { Result, SessionDetail, SessionSummary } from './protocol.ts';

const replySchema = z.object({
  id: z.number().optional(),
  result: z.unknown().optional(),
  error: z.object({ code: z.number(), message: z.string() }).optional(),
});

export interface Sessions {
  list(cursor?: string): Promise<Result<Readonly<{ sessions: readonly SessionSummary[]; nextCursor: string | null }>>>;
  read(id: string): Promise<Result<SessionDetail>>;
}

export class CodexSessions implements Sessions {
  private socket: WebSocket | null = null;
  private connecting: Promise<Result<true>> | null = null;
  private nextId = 0;
  private readonly pending = new Map<number, (value: unknown) => void>();
  readonly socketPath: string;
  readonly provider: string;

  constructor(socketPath: string, provider: string) {
    this.socketPath = socketPath;
    this.provider = provider;
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
        const parsed = replySchema.safeParse(JSON.parse(data.toString()));
        if (!parsed.success || parsed.data.id === undefined) return;
        this.pending.get(parsed.data.id)?.(parsed.data);
        this.pending.delete(parsed.data.id);
      } catch {}
    });
    socket.on('error', () => {});
    socket.on('close', () => {
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
    if (reply.data.error) return failure(502, `Codex could not read this session (${reply.data.error.code}).`);
    const parsed = schema.safeParse(reply.data.result);
    return parsed.success ? { ok: true, value: parsed.data } : failure(502, 'This Codex response is not supported yet.');
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
        sessions: page.value.data.map(thread => sessionSummary(thread, ['active', 'idle'].includes(thread.status.type))),
        nextCursor: page.value.nextCursor ?? null,
      },
    };
  }

  async read(id: string): Promise<Result<SessionDetail>> {
    if (!z.string().uuid().safeParse(id).success) return failure(404, 'Session not found.');
    const connected = await this.connect();
    if (!connected.ok) return connected;
    const metadata = await this.request('thread/read', { threadId: id }, z.object({ thread: threadSchema }));
    if (!metadata.ok) return metadata;
    if (metadata.value.thread.modelProvider !== this.provider) return failure(404, 'Session not found.');
    const turns = await this.request('thread/turns/list', {
      threadId: id, limit: 12, sortDirection: 'desc', itemsView: 'full',
    }, turnPageSchema);
    if (!turns.ok) return turns;
    const thread = metadata.value.thread;
    return {
      ok: true,
      value: {
        session: sessionSummary(thread, ['active', 'idle'].includes(thread.status.type)),
        messages: turns.value.data.toReversed().flatMap(turn => turn.items.flatMap(item => visibleMessage(item, turn.id))),
        olderMessages: Boolean(turns.value.nextCursor),
      },
    };
  }

  close(): void { this.socket?.close(); }
}
