import { z } from 'zod';
import { redact } from './protocol.ts';
import type { Message, SessionSummary } from './protocol.ts';

export const threadSchema = z.object({
  id: z.string(),
  name: z.string().nullish(),
  preview: z.string().default(''),
  cwd: z.string(),
  modelProvider: z.string(),
  model: z.string().nullish(),
  canAcceptDirectInput: z.boolean().nullish(),
  createdAt: z.number(),
  updatedAt: z.number(),
  status: z.object({ type: z.string(), activeFlags: z.array(z.string()).optional() }),
  source: z.unknown().optional(),
}).readonly();
export type Thread = z.infer<typeof threadSchema>;
export const threadPageSchema = z.object({ data: z.array(threadSchema), nextCursor: z.string().nullish() });
export const itemSchema = z.object({
  id: z.string().default(''),
  type: z.string(),
  text: z.string().optional(),
  command: z.string().optional(),
  status: z.string().optional(),
  content: z.array(z.object({ type: z.string(), text: z.string().optional() })).optional(),
  changes: z.array(z.object({ path: z.string() })).optional(),
});
export const turnSchema = z.object({
  id: z.string(), status: z.string(), items: z.array(itemSchema).default([]),
  error: z.object({ message: z.string() }).nullish(),
});
export const turnPageSchema = z.object({ data: z.array(turnSchema), nextCursor: z.string().nullish() });
export function visibleMessage(item: z.infer<typeof itemSchema>, turnId: string): readonly Message[] {
  const id = `${turnId}/${item.id}`;
  switch (item.type) {
    case 'userMessage': {
      const text = item.content?.filter(part => part.type === 'text').map(part => part.text ?? '').join('\n') ?? '';
      return text ? [{ id, role: 'user', text: redact(text) }] : [];
    }
    case 'agentMessage':
      return item.text ? [{ id, role: 'assistant', text: redact(item.text) }] : [];
    case 'commandExecution':
      return [{ id, role: 'activity', text: redact(item.command ?? 'Running a command') }];
    case 'fileChange':
      return [{ id, role: 'activity', text: `Edited ${item.changes?.map(change => change.path).join(', ') || 'files'}` }];
    default:
      return [];
  }
}

export function sessionSummary(thread: Thread, live = false): SessionSummary {
  return {
    id: thread.id,
    title: redact(thread.name || thread.preview.split('\n')[0] || 'Untitled session').slice(0, 140),
    project: thread.cwd.split('/').filter(Boolean).at(-1) ?? thread.cwd,
    cwd: thread.cwd,
    agent: 'codex',
    model: thread.model ?? null,
    status: thread.status.activeFlags?.includes('waitingOnApproval') ? 'needs_approval'
      : thread.status.activeFlags?.includes('waitingOnUserInput') ? 'needs_input'
      : thread.status.type === 'active' ? 'working' : live ? 'ready' : 'saved',
    canSend: thread.canAcceptDirectInput !== false,
    canStop: thread.status.type === 'active',
    updatedAt: thread.updatedAt,
  } as const;
}
