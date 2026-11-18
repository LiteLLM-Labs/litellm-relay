import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdir, rm } from 'node:fs/promises';
import { resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { WebSocketServer } from 'ws';
import { z } from 'zod';
import { CodexSessions } from './codex.ts';

test('the Codex adapter shows new conversation content, omits hidden reasoning, and refuses other providers', async t => {
  const directory = resolve('.local/tests', randomUUID());
  await mkdir(directory, { recursive: true });
  t.after(() => rm(directory, { recursive: true, force: true }));
  const path = resolve(directory, 'codex.sock');
  const server = createServer();
  const websocket = new WebSocketServer({ server });
  const sessionId = randomUUID();
  const state = { text: 'First response', provider: 'gateway' };
  const thread = () => ({ id: sessionId, name: 'A live task', preview: 'A live task', cwd: '/workspace/project',
    modelProvider: state.provider, createdAt: 1, updatedAt: 2, status: { type: 'active', activeFlags: [] } });
  websocket.on('connection', socket => socket.on('message', raw => {
    const request = z.object({ id: z.number().optional(), method: z.string() }).parse(JSON.parse(raw.toString()));
    if (request.id === undefined) return;
    const response = (() => {
      switch (request.method) {
        case 'initialize': return { userAgent: 'test' };
        case 'thread/list': return { data: [thread()], nextCursor: null };
        case 'thread/read': return { thread: thread() };
        case 'thread/turns/list': return { data: [{ id: 'turn', status: 'completed', items: [
          { id: 'user', type: 'userMessage', content: [{ type: 'text', text: 'Check this task' }] },
          { id: 'reasoning', type: 'reasoning', text: 'Private reasoning' },
          { id: 'tool', type: 'commandExecution', command: 'npm test', aggregatedOutput: 'private-tool-output' },
          { id: 'assistant', type: 'agentMessage', text: state.text },
        ] }], nextCursor: null };
        default: return {};
      }
    })();
    socket.send(JSON.stringify({ id: request.id, result: response }));
  }));
  await new Promise<void>(resolve => server.listen(path, resolve));
  t.after(() => { for (const client of websocket.clients) client.terminate(); websocket.close(); server.close(); });
  const adapter = new CodexSessions(path, 'gateway');
  t.after(() => adapter.close());
  const listed = await adapter.list();
  assert(listed.ok);
  assert.equal(listed.value.sessions[0].id, sessionId);
  assert.equal(listed.value.sessions[0].live, true);
  const first = await adapter.read(sessionId);
  assert(first.ok);
  assert.equal(first.value.messages.at(-1)?.text, state.text);
  assert.equal(first.value.messages.some(message => message.text.includes('Private reasoning')), false);
  assert.equal(first.value.messages.some(message => message.text.includes('private-tool-output')), false);
  state.text = 'A later response';
  const second = await adapter.read(sessionId);
  assert(second.ok);
  assert.equal(second.value.messages.at(-1)?.text, state.text);
  state.provider = 'another-provider';
  const refused = await adapter.read(sessionId);
  assert(!refused.ok && refused.status === 404);
  assert.equal((await adapter.read('../another-file')).ok, false);
});
