import { z } from 'zod';

export type Result<T> = Readonly<{ ok: true; value: T }> | Readonly<{ ok: false; status: number; error: string }>;
export type Identity = Readonly<{ subject: string; label: string; method: 'sso' | 'gateway'; expiresAt: number }>;
export const failure = (status: number, error: string): Result<never> => ({ ok: false, status, error });

export type Message = Readonly<{ id: string; role: 'user' | 'assistant' | 'activity'; text: string }>;

export function redact(text: string): string {
  return text
    .replace(/\bsk-[A-Za-z0-9_-]{16,}/g, '[credential redacted]')
    .replace(/\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+/g, '[token redacted]');
}

export function publicError(message: string): string {
  const parsed = (() => {
    try { return z.object({ error: z.object({ message: z.string() }) }).safeParse(JSON.parse(message)); }
    catch { return null; }
  })();
  const text = parsed?.success ? parsed.data.error.message : message;
  if (/no healthy deployments/i.test(text)) return 'This model is unavailable on your gateway. Choose another model or try again later.';
  return redact(text.replace(/^litellm\.[A-Za-z]+Error:\s*/, '').split('\n\n')[0]).slice(0, 600);
}

export type SessionDetail = Readonly<{
  session: SessionSummary;
  messages: readonly Message[];
  olderMessages: boolean;
  error?: string;
  approvals: readonly Approval[];
}>;

export type Approval = Readonly<{ id: string; title: string; text: string; canAccept: boolean }>;
export const messageInput = z.object({ requestId: z.string().uuid(), text: z.string().trim().min(1).max(32000), model: z.string().trim().min(1).max(200).optional() }).strict();
export const agentKind = z.enum(['codex', 'claude']);
export type AgentKind = z.infer<typeof agentKind>;
export const sessionIdSchema = z.string().regex(/^(codex|claude):[a-f0-9-]{36}$/);
export const newSessionInput = z.object({ requestId: z.string().uuid(), cwd: z.string().min(1).max(4096), agent: agentKind }).strict();
export const approvalInput = z.object({ decision: z.enum(['accept', 'decline']) }).strict();
export type ModelPage = Readonly<{ models: readonly Readonly<{ id: string; label: string; isDefault: boolean }>[]; nextCursor: string | null }>;

export type SessionSummary = Readonly<{
  id: string; agent: AgentKind; title: string; project: string; cwd: string; model: string | null;
  status: 'working' | 'ready' | 'needs_approval' | 'needs_input' | 'saved' | 'handoff_required' | 'error';
  canSend: boolean; canStop: boolean; updatedAt: number;
}>;
export interface Sessions {
  list(cursor?: string): Promise<Result<Readonly<{ sessions: readonly SessionSummary[]; nextCursor: string | null; warnings?: readonly string[] }>>>;
  read(id: string): Promise<Result<SessionDetail>>;
  create(cwd: string, agent?: AgentKind): Promise<Result<SessionSummary>>;
  models(cursor?: string): Promise<Result<ModelPage>>;
  send(id: string, text: string, model?: string): Promise<Result<Readonly<{ turnId: string }>>>;
  approve(id: string, approvalId: string, decision: 'accept' | 'decline'): Promise<Result<Readonly<{ resolved: boolean }>>>;
  interrupt(id: string): Promise<Result<Readonly<{ stopped: boolean }>>>;
}
