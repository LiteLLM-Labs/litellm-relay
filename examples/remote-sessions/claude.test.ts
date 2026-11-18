import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir, rm } from 'node:fs/promises';
import { resolve } from 'node:path';
import { ClaudeSessions } from './claude.ts';
import { visibleClaudeContent } from './claude-runtime.ts';
import type { ClaudeRuntime } from './claude-runtime.ts';
import type { Message } from './protocol.ts';
import { failure } from './protocol.ts';

function fixture(directory: string) {
  const id = randomUUID();
  const info = { id, cwd: directory, title: 'Existing conversation', updatedAt: 1 };
  const state: { active: boolean; activeId?: string; opens: number; sends: number; model?: string;
    options?: Parameters<ClaudeRuntime['open']>[0]; history: readonly Message[] } = {
    active: true, opens: 0, sends: 0, history: [{ id: 'previous', role: 'user', text: 'Remember this work' }],
  };
  const runtime: ClaudeRuntime = {
    active: async () => ({ ok: true, value: state.active ? [{ ...info, id: state.activeId ?? id }] : [] }),
    info: async value => value === id ? { ok: true, value: info } : failure(404, 'Not saved yet'),
    history: async () => ({ ok: true, value: state.history }),
    open: async options => {
      state.opens += 1;
      state.options = options;
      return { ok: true, value: {
        send: async (_text, model) => {
          state.sends += 1;
          state.model = model;
          options.event({ type: 'init', id: options.id, model: model ?? options.model });
          return { ok: true, value: { turnId: randomUUID() } };
        },
        interrupt: async () => { options.event({ type: 'done', error: 'Interrupted' }); return { ok: true, value: { stopped: true } }; },
        close: () => options.event({ type: 'closed' }),
      } };
    },
  };
  return { id, state, runtime };
}

async function testDirectory(cleanup: (fn: () => Promise<void>) => void): Promise<string> {
  const directory = resolve('.local/tests', randomUUID());
  await mkdir(directory, { recursive: true });
  cleanup(() => rm(directory, { recursive: true, force: true }));
  return directory;
}

test('Claude discovery never grants control and an explicit idle Mac handoff survives a connector restart', async t => {
  const directory = await testDirectory(fn => t.after(fn));
  const { id, state, runtime } = fixture(directory);
  const sessions = new ClaudeSessions(runtime, `${directory}/grants`);
  t.after(() => sessions.close());
  const list = await sessions.list();
  assert(list.ok);
  assert.equal(list.value.sessions[0].status, 'handoff_required');
  assert.equal(list.value.sessions[0].canSend, false);
  assert.equal((await sessions.send(id, 'Must not run')).ok, false);
  assert.equal((await sessions.handoff(id)).ok, false);
  const preview = await sessions.read(id);
  assert(preview.ok);
  assert.equal(preview.value.messages.length, 0);
  assert.equal(state.opens, 0);
  state.active = false;
  assert((await sessions.handoff(id)).ok);
  const restarted = new ClaudeSessions(runtime, `${directory}/grants`);
  t.after(() => restarted.close());
  const detail = await restarted.read(id);
  assert(detail.ok);
  assert.equal(detail.value.messages[0].text, state.history[0].text);
  assert.equal(detail.value.session.canSend, true);
  state.active = true;
  assert.equal((await restarted.send(id, 'Must not create a second writer')).ok, false);
  assert.equal(state.opens, 0);
  state.active = false;
  assert((await restarted.send(id, 'Continue the same session', 'gateway-model')).ok);
  assert.equal(state.options?.id, id);
  assert.equal(state.options?.resume, true);
  assert.equal(state.model, 'gateway-model');
  restarted.close();
  const restored = await new ClaudeSessions(runtime, `${directory}/grants`).read(id);
  assert(restored.ok);
  assert.equal(restored.value.session.model, 'gateway-model');
});

test('Claude streams visible replies, scopes one-time approvals and clears cancelled requests when a turn stops', async t => {
  const directory = await testDirectory(fn => t.after(fn));
  const { id, state, runtime } = fixture(directory);
  state.active = false;
  const sessions = new ClaudeSessions(runtime, `${directory}/grants`);
  t.after(() => sessions.close());
  assert((await sessions.handoff(id)).ok);
  assert((await sessions.send(id, 'Continue')).ok);
  const options = state.options;
  assert(options);
  options.event({ type: 'message', append: true, message: { id: 'reply', role: 'assistant', text: 'First ' } });
  options.event({ type: 'message', append: true, message: { id: 'reply', role: 'assistant', text: 'second' } });
  const streamed = await sessions.read(id);
  assert(streamed.ok);
  assert.equal(streamed.value.messages.find(value => value.id === 'reply')?.text, 'First second');
  const request = options.approve('Bash', { command: 'npm test' }, new AbortController().signal);
  const pending = await sessions.read(id);
  assert(pending.ok);
  assert.equal(pending.value.session.status, 'needs_approval');
  const approval = pending.value.approvals[0];
  assert(approval.text.includes('npm test'));
  assert.equal((await sessions.approve(randomUUID(), approval.id, 'accept')).ok, false);
  assert((await sessions.approve(id, approval.id, 'accept')).ok);
  assert.equal(await request, true);
  assert.equal((await sessions.approve(id, approval.id, 'accept')).ok, false);
  const cancellation = new AbortController();
  const cancelled = options.approve('Write', { file_path: `${directory}/output`, content: 'Change' }, cancellation.signal);
  cancellation.abort();
  assert.equal(await cancelled, false);
  const large = options.approve('Edit', { content: 'x'.repeat(33000) }, new AbortController().signal);
  const limited = await sessions.read(id);
  assert(limited.ok);
  assert.equal(limited.value.approvals[0].canAccept, false);
  assert.equal((await sessions.approve(id, limited.value.approvals[0].id, 'accept')).ok, false);
  assert((await sessions.interrupt(id)).ok);
  assert.equal(await large, false);
  const stopped = await sessions.read(id);
  assert(stopped.ok);
  assert.equal(stopped.value.approvals.length, 0);
  assert.equal(stopped.value.session.canSend, true);
  assert.equal(stopped.value.error, undefined);
});

test('new Claude sessions cannot double-send, silently fork, or lose errors when their runtime exits', async t => {
  const directory = await testDirectory(fn => t.after(fn));
  const { state, runtime } = fixture(directory);
  state.active = false;
  state.history = [];
  const sessions = new ClaudeSessions(runtime, `${directory}/grants`);
  t.after(() => sessions.close());
  const created = await sessions.create(directory);
  assert(created.ok);
  const id = created.value.id;
  const results = await Promise.all([sessions.send(id, 'First'), sessions.send(id, 'Duplicate')]);
  assert.equal(results.filter(value => value.ok).length, 1);
  assert.equal(state.sends, 1);
  assert.equal(state.options?.resume, false);
  const options = state.options;
  assert(options);
  options.event({ type: 'init', id: randomUUID(), model: 'unexpected' });
  await new Promise<void>(resolve => setImmediate(resolve));
  const mismatch = await sessions.read(id);
  assert(mismatch.ok);
  assert.equal(mismatch.value.session.status, 'error');
  assert.match(mismatch.value.error ?? '', /different session/);
  assert.equal(state.sends, 1);
  options.event({ type: 'closed', error: 'Gateway disconnected' });
  const failed = await sessions.read(id);
  assert(failed.ok);
  assert.equal(failed.value.error, 'Gateway disconnected');
  assert((await sessions.send(id, 'Retry after reviewing the error')).ok);
  assert.equal(state.opens, 2);
  state.options?.event({ type: 'closed' });
  state.active = true;
  state.activeId = id;
  const outside = await sessions.list();
  assert(outside.ok);
  assert.equal(outside.value.sessions.find(value => value.id === id)?.canSend, false);
  sessions.close();
  state.active = false;
  assert.equal((await sessions.send(id, 'Do not launch during shutdown')).ok, false);
  assert.equal(state.opens, 2);
});

test('Claude exposes only visible text and tool activity while redacting credentials', () => {
  const key = 'sk-' + 'private'.repeat(6);
  const messages = visibleClaudeContent({ id: 'assistant', content: [
    { type: 'thinking', thinking: 'Internal reasoning' },
    { type: 'text', text: `Public reply ${key}` },
    { type: 'tool_use', id: 'command', name: 'Bash', input: { command: 'npm test' } },
    { type: 'redacted_thinking', data: 'Opaque reasoning' },
  ] }, 'message', 'assistant');
  assert.equal(messages.length, 2);
  assert.equal(messages[0].text, 'Public reply [credential redacted]');
  assert.equal(messages[1].role, 'activity');
  assert.match(messages[1].text, /npm test/);
  assert.equal(visibleClaudeContent({ content: [{ type: 'tool_result', content: key }] }, 'result', 'user').length, 0);
});
