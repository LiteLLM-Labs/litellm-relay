import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { TransformStream } from 'node:stream/web';
import { randomUUID } from 'node:crypto';
import { query, getSessionMessages, getSessionInfo, resolveSettings } from '@anthropic-ai/claude-agent-sdk';
import type { PermissionResult, SDKUserMessage } from '@anthropic-ai/claude-agent-sdk';
import { z } from 'zod';
import { failure, publicError, redact } from './protocol.ts';
import type { Message, Result } from './protocol.ts';

const execute = promisify(execFile);
const activeSchema = z.array(z.object({ sessionId: z.string().optional(), cwd: z.string(), name: z.string().optional(),
  pid: z.number().optional(), status: z.string().optional() }));
const contentSchema = z.object({ id: z.string().optional(), content: z.union([z.string(), z.array(z.object({ type: z.string(), text: z.string().optional(),
  name: z.string().optional(), input: z.unknown().optional(), id: z.string().optional() }))]) });

export type ClaudeInfo = Readonly<{ id: string; cwd: string; title: string; updatedAt: number }>;
export type ClaudeEvent = Readonly<{ type: 'init'; id: string; model: string }>
  | Readonly<{ type: 'message'; message: Message; append: boolean }>
  | Readonly<{ type: 'done'; error?: string }>
  | Readonly<{ type: 'closed'; error?: string }>;
export type ClaudeRun = Readonly<{
  send: (text: string, model?: string) => Promise<Result<Readonly<{ turnId: string }>>>;
  interrupt: () => Promise<Result<Readonly<{ stopped: boolean }>>>;
  close: () => void;
}>;
export interface ClaudeRuntime {
  active(): Promise<Result<readonly Readonly<{ id: string; cwd: string; title: string; updatedAt: number }>[]>>;
  info(id: string): Promise<Result<ClaudeInfo>>;
  history(id: string, cwd: string): Promise<Result<readonly Message[]>>;
  open(options: Readonly<{ id: string; cwd: string; model: string; resume: boolean;
    event: (event: ClaudeEvent) => void;
    approve: (tool: string, input: Readonly<Record<string, unknown>>, signal: AbortSignal) => Promise<boolean>;
  }>): Promise<Result<ClaudeRun>>;
}

export async function findClaudeExecutable(): Promise<string> {
  if (process.env.CLAUDE_EXECUTABLE) return process.env.CLAUDE_EXECUTABLE;
  const found = await execute('which', ['claude'], { timeout: 3000 }).catch(() => null);
  return found?.stdout.trim() || 'claude';
}

export function visibleClaudeContent(message: unknown, id: string, role: 'user' | 'assistant'): readonly Message[] {
  const parsed = contentSchema.safeParse(message);
  if (!parsed.success) return [];
  const content = parsed.data.content;
  const messageId = role === 'assistant' ? parsed.data.id ?? id : id;
  if (typeof content === 'string') return [{ id, role, text: redact(content).slice(0, 32000) }];
  if (role === 'user') {
    const text = content.filter(block => block.type === 'text').map(block => block.text ?? '').join('\n');
    return text ? [{ id, role, text: redact(text).slice(0, 32000) }] : [];
  }
  return content.flatMap<Message>((block, index) => {
    if (block.type === 'text' && block.text) return [{ id: `${messageId}/${index}`, role, text: redact(block.text).slice(0, 32000) }];
    if (role === 'assistant' && block.type === 'tool_use') return [{ id: block.id ?? `${id}/${index}`, role: 'activity' as const,
      text: redact(`${block.name ?? 'Tool'}\n${JSON.stringify(block.input ?? {})}`).slice(0, 32000) }];
    return [];
  });
}

export class SdkClaudeRuntime implements ClaudeRuntime {
  private readonly executable: string;
  private readonly gateway: string;

  constructor(executable: string, gateway: string) { this.executable = executable; this.gateway = gateway; }

  async active(): ReturnType<ClaudeRuntime['active']> {
    const result = await execute(this.executable, ['agents', '--json'], { timeout: 5000, maxBuffer: 1024 * 1024 }).catch(() => null);
    if (!result) return failure(503, 'Cannot check running Claude sessions. Install Claude Code and configure Relay on this Mac.');
    const parsed = (() => { try { return activeSchema.safeParse(JSON.parse(result.stdout)); } catch { return null; } })();
    if (!parsed?.success) return failure(502, 'This Claude version does not support session discovery.');
    return { ok: true, value: parsed.data.filter(session => session.pid && session.sessionId)
      .map(session => ({ id: session.sessionId!, cwd: session.cwd, title: redact(session.name ?? 'Claude session'), updatedAt: Math.floor(Date.now() / 1000) })) };
  }

  async info(id: string): ReturnType<ClaudeRuntime['info']> {
    try {
      const info = await getSessionInfo(id);
      if (!info?.cwd) return failure(404, 'Claude session not found.');
      return { ok: true, value: { id: info.sessionId, cwd: info.cwd, title: redact(info.customTitle || info.summary || 'Claude session'), updatedAt: Math.floor(info.lastModified / 1000) } };
    } catch { return failure(502, 'Claude session metadata could not be read.'); }
  }

  async history(id: string, cwd: string): ReturnType<ClaudeRuntime['history']> {
    const messages = await getSessionMessages(id, { dir: cwd }).catch(() => null);
    if (!messages) return failure(502, 'Claude history could not be read.');
    return { ok: true, value: messages.filter(message => !message.parent_tool_use_id)
      .flatMap(message => message.type === 'user' || message.type === 'assistant' ? visibleClaudeContent(message.message, message.uuid, message.type) : []) };
  }

  async open(options: Parameters<ClaudeRuntime['open']>[0]): ReturnType<ClaudeRuntime['open']> {
    const resolved = await resolveSettings({ cwd: options.cwd, settingSources: ['user', 'project', 'local'] }).catch(() => null);
    if (!resolved) return failure(502, 'Claude settings could not be loaded.');
    const settings = z.object({ env: z.record(z.string(), z.string()).optional() }).safeParse(resolved.effective);
    if (!settings.success || (resolved.machinePolicy?.didNotLoad && resolved.machinePolicy.didNotLoad.length > 0)) return failure(503, 'Claude settings or managed policy could not be verified. Check Relay on this Mac.');
    const environment = settings.data.env ?? {};
    const normalize = (value: string) => value.replace(/\/+$/, '').replace(/\/v1$/, '');
    const configured = environment.ANTHROPIC_BASE_URL;
    if (configured && normalize(configured) !== normalize(this.gateway)) return failure(409, 'Configure this project’s Claude endpoint to use the connected LiteLLM gateway before handing it over.');
    if (['CLAUDE_CODE_USE_BEDROCK', 'CLAUDE_CODE_USE_VERTEX', 'CLAUDE_CODE_USE_FOUNDRY'].some(name => environment[name] === '1')) return failure(409, 'This project selects another provider. Configure Claude to use the connected LiteLLM gateway.');
    const input = new TransformStream<SDKUserMessage, SDKUserMessage>();
    const writer = input.writable.getWriter();
    const runner = query({ prompt: input.readable, options: {
      cwd: options.cwd, ...(options.resume ? { resume: options.id } : { sessionId: options.id }), model: options.model,
      pathToClaudeCodeExecutable: this.executable, permissionMode: 'default', includePartialMessages: true,
      settingSources: ['user', 'project', 'local'], mcpServersDelivery: 'argv',
      env: { ...process.env, ANTHROPIC_BASE_URL: this.gateway,
        ...(process.env.LITELLM_API_KEY ? { ANTHROPIC_API_KEY: undefined, ANTHROPIC_AUTH_TOKEN: process.env.LITELLM_API_KEY } : {}),
        CLAUDE_CODE_USE_BEDROCK: undefined, CLAUDE_CODE_USE_VERTEX: undefined, CLAUDE_CODE_USE_FOUNDRY: undefined,
        DISABLE_AUTOUPDATER: '1', CLAUDE_AGENT_SDK_CLIENT_APP: 'litellm-relay-remote/0.1.0' },
      canUseTool: async (tool, value, request): Promise<PermissionResult> => await options.approve(tool, value, request.signal)
        ? { behavior: 'allow', updatedInput: value } : { behavior: 'deny', message: 'The user declined this request.' },
    } });
    async function consume(): Promise<void> {
      let blockId = '';
      try {
        for await (const event of runner) {
          if (event.type === 'system' && event.subtype === 'init') options.event({ type: 'init', id: event.session_id, model: event.model });
          if (event.type === 'assistant') for (const message of visibleClaudeContent(event.message, event.uuid, 'assistant')) options.event({ type: 'message', message, append: false });
          if (event.type === 'stream_event') {
            const partial = event.event;
            if (partial.type === 'message_start') blockId = partial.message.id;
            if (partial.type === 'content_block_delta' && partial.delta.type === 'text_delta') options.event({ type: 'message',
              message: { id: `${blockId}/${partial.index}`, role: 'assistant', text: partial.delta.text }, append: true });
          }
          if (event.type === 'result') options.event({ type: 'done', error: event.subtype !== 'success' ? publicError(event.errors.join('\n')) : undefined });
        }
        options.event({ type: 'closed' });
      } catch (error) { options.event({ type: 'closed', error: publicError(error instanceof Error ? error.message : 'Claude disconnected.') }); }
    }
    void consume();
    const timer = setTimeout(() => runner.close(), 10000);
    const initialized = await runner.initializationResult().then(() => true).catch(() => false);
    clearTimeout(timer);
    if (!initialized) { runner.close(); return failure(503, 'Claude could not start. Check its configuration on this Mac.'); }
    return { ok: true, value: {
      send: async (text, model) => {
        try {
          if (model) await runner.setModel(model);
          const turnId = randomUUID();
          await writer.write({ type: 'user', session_id: options.id, parent_tool_use_id: null, uuid: turnId,
            message: { role: 'user', content: text } });
          return { ok: true, value: { turnId } };
        } catch { return failure(502, 'Claude did not confirm the message. Refresh before sending again.'); }
      },
      interrupt: async () => {
        try { await runner.interrupt(); return { ok: true, value: { stopped: true } }; }
        catch { return failure(502, 'Claude did not confirm the stop request.'); }
      },
      close: () => runner.close(),
    } };
  }
}
