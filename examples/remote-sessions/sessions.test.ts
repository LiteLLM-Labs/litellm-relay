import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { RemoteSessions } from './sessions.ts';
import type { AgentKind, SessionSummary, Sessions } from './protocol.ts';

test('the shared API routes colliding agent IDs, continuations, approvals and stops to the selected agent', async () => {
  const id = randomUUID();
  const calls: string[] = [];
  const adapter = (agent: AgentKind): Sessions => {
    const session: SessionSummary = { id, agent, title: 'Task', project: 'project', cwd: '/project', model: null,
      status: 'ready', canSend: true, canStop: false, updatedAt: 1 };
    return {
      list: async () => ({ ok: true, value: { sessions: [session], nextCursor: null } }),
      read: async value => {
        calls.push(`${agent}/read/${value}`);
        return { ok: true, value: { session, messages: [], olderMessages: false, approvals: [] } };
      },
      create: async () => ({ ok: true, value: session }),
      send: async (value, text, model) => { calls.push(`${agent}/send/${value}/${text}/${model}`); return { ok: true, value: { turnId: 'turn' } }; },
      approve: async (value, approval, decision) => { calls.push(`${agent}/approve/${value}/${approval}/${decision}`); return { ok: true, value: { resolved: true } }; },
      interrupt: async value => { calls.push(`${agent}/stop/${value}`); return { ok: true, value: { stopped: true } }; },
      models: async () => ({ ok: true, value: { models: [], nextCursor: null } }),
    };
  };
  const sessions = new RemoteSessions({ claude: adapter('claude'), codex: adapter('codex') });
  const page = await sessions.list();
  assert(page.ok);
  assert.equal(new Set(page.value.sessions.map(value => value.id)).size, 2);
  for (const agent of ['codex', 'claude'] as const) {
    const publicId = `${agent}:${id}`;
    const created = await sessions.create('/project', agent);
    assert(created.ok);
    assert.equal(created.value.id, publicId);
    const read = await sessions.read(publicId);
    assert(read.ok);
    assert.equal(read.value.session.id, publicId);
    assert((await sessions.send(publicId, 'Continue', 'selected-model')).ok);
    assert((await sessions.approve(publicId, 'approval', 'decline')).ok);
    assert((await sessions.interrupt(publicId)).ok);
    assert.deepEqual(calls.slice(-4), [`${agent}/read/${id}`, `${agent}/send/${id}/Continue/selected-model`,
      `${agent}/approve/${id}/approval/decline`, `${agent}/stop/${id}`]);
  }
  const count = calls.length;
  assert.equal((await sessions.send(`unknown:${id}`, 'Do not route this')).ok, false);
  assert.equal((await sessions.send(id, 'Do not guess the agent')).ok, false);
  assert.equal(calls.length, count);
});
