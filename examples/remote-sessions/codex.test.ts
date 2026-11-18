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
  const adapter = new CodexSessions(path, 'gateway', 'https://gateway.example.com');
  t.after(() => adapter.close());
  const listed = await adapter.list();
  assert(listed.ok);
  assert.equal(listed.value.sessions[0].id, sessionId);
  assert.equal(listed.value.sessions[0].status, 'working');
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

test('phone sessions use the gateway, continue the same history, and require explicit scoped approvals', async t => {
  const directory = resolve('.local/tests', randomUUID());
  await mkdir(directory, { recursive: true });
  t.after(() => rm(directory, { recursive: true, force: true }));
  const path = resolve(directory, 'codex.sock');
  const server = createServer();
  const websocket = new WebSocketServer({ server });
  const id = randomUUID();
  const choices = [
    { model: 'gateway-fast', displayName: 'Fast model', isDefault: true, hidden: false },
    { model: 'gateway-reasoning', displayName: 'Reasoning model', isDefault: false, hidden: false },
    { model: 'hidden-option', displayName: 'Hidden model', isDefault: false, hidden: true },
  ];
  const state = { provider: 'gateway', endpoint: 'https://gateway.example.com/v1', text: '', turns: 0, starts: 0,
    interrupts: 0, decision: '', policy: 'on-request', sandbox: 'workspace-write', persisted: false, model: choices[0].model, error: '' };
  const thread = () => ({ id, cwd: directory, modelProvider: state.provider, createdAt: 1, updatedAt: 2,
    model: state.model, status: { type: state.turns ? 'active' : 'idle', activeFlags: [] } });
  const client = new Promise<import('ws').WebSocket>(resolve => websocket.once('connection', resolve));
  websocket.on('connection', socket => socket.on('message', raw => {
    const request = z.object({ id: z.union([z.number(), z.string()]).optional(), method: z.string().optional(),
      params: z.record(z.string(), z.unknown()).optional(), result: z.object({ decision: z.string() }).optional(),
    }).parse(JSON.parse(raw.toString()));
    if (request.result) { state.decision = request.result.decision; return; }
    if (request.id === undefined) return;
    if (!state.persisted && ['thread/read', 'thread/resume', 'thread/turns/list'].includes(request.method ?? '')) {
      socket.send(JSON.stringify({ id: request.id, error: { code: -32600, message: 'No saved history before the first turn' } }));
      return;
    }
    const response = (() => {
      switch (request.method) {
        case 'initialize': return { userAgent: 'test' };
        case 'config/read': return { config: { model_providers: { gateway: { base_url: state.endpoint } } } };
        case 'model/list': {
          assert.equal(request.params?.modelProvider, 'gateway');
          return { data: choices, nextCursor: null };
        }
        case 'thread/start': {
          state.starts += 1;
          state.provider = z.string().parse(request.params?.modelProvider);
          state.policy = z.string().parse(request.params?.approvalPolicy);
          state.sandbox = z.string().parse(request.params?.sandbox);
          return { thread: thread() };
        }
        case 'thread/resume': {
          state.policy = z.string().optional().parse(request.params?.approvalPolicy) ?? state.policy;
          state.sandbox = z.string().optional().parse(request.params?.sandbox) ?? state.sandbox;
          return { thread: thread() };
        }
        case 'thread/read': return { thread: thread() };
        case 'thread/turns/list': return { data: state.turns ? [{ id: 'turn', status: 'inProgress', error: state.error ? { message: state.error } : null, items: [
          { id: 'user', type: 'userMessage', content: [{ type: 'text', text: state.text }] },
        ] }] : [], nextCursor: null };
        case 'turn/interrupt': {
          assert.equal(request.params?.threadId, id);
          assert.equal(request.params?.turnId, 'turn');
          state.interrupts += 1;
          return {};
        }
        case 'turn/start': {
          assert.equal(request.params?.threadId, id);
          state.text = z.array(z.object({ text: z.string() })).parse(request.params?.input)[0].text;
          state.model = z.string().optional().parse(request.params?.model) ?? state.model;
          state.turns += 1;
          return { turn: { id: `turn-${state.turns}` } };
        }
        default: return {};
      }
    })();
    socket.send(JSON.stringify({ id: request.id, result: response }));
  }));
  await new Promise<void>(resolve => server.listen(path, resolve));
  t.after(() => { for (const socket of websocket.clients) socket.terminate(); websocket.close(); server.close(); });
  const adapter = new CodexSessions(path, 'gateway', 'https://gateway.example.com');
  t.after(() => adapter.close());
  const created = await adapter.create(directory);
  assert(created.ok);
  assert.equal(created.value.agent, 'codex');
  assert.equal(state.policy, 'on-request');
  assert.equal(state.sandbox, 'workspace-write');
  const empty = await adapter.read(created.value.id);
  assert(empty.ok);
  assert.equal(empty.value.messages.length, 0);
  const catalog = await adapter.models();
  assert(catalog.ok);
  assert.deepEqual(catalog.value.models.map(model => model.id), choices.filter(model => !model.hidden).map(model => model.model));
  assert((await adapter.send(id, 'First phone message', choices[1].model)).ok);
  const persisting = await adapter.read(id);
  assert(persisting.ok);
  assert.equal(persisting.value.messages[0].text, 'First phone message');
  assert.equal(persisting.value.session.model, choices[1].model);
  assert.equal(persisting.value.session.status, 'working');
  assert.equal((await adapter.send(id, 'Cannot silently switch mid-reply', choices[0].model)).ok, false);
  state.persisted = true;
  assert((await adapter.send(id, 'Continue this same session')).ok);
  assert.equal(state.turns, 2);
  assert.equal(state.starts, 1);
  assert.equal(state.policy, 'on-request');
  assert.equal(state.sandbox, 'workspace-write');
  assert.equal(state.model, choices[1].model);
  const stream = await client;
  stream.send(JSON.stringify({ method: 'item/agentMessage/delta', params: { threadId: id, turnId: 'turn', itemId: 'reply', delta: 'An incremental' } }));
  const partial = await adapter.read(id);
  assert(partial.ok);
  assert.equal(partial.value.messages.at(-1)?.text, 'An incremental');
  stream.send(JSON.stringify({ method: 'item/agentMessage/delta', params: { threadId: id, turnId: 'turn', itemId: 'reply', delta: ' response' } }));
  const continued = await adapter.read(id);
  assert(continued.ok);
  assert.equal(continued.value.messages.at(-1)?.text, 'An incremental response');
  state.error = JSON.stringify({ error: { message: 'No healthy deployments are currently available.' } });
  const failedTurn = await adapter.read(id);
  assert(failedTurn.ok);
  assert.equal(failedTurn.value.error, 'This model is unavailable on your gateway. Choose another model or try again later.');
  state.error = '';
  const detail = await adapter.read(id);
  assert(detail.ok);
  assert.equal(detail.value.messages[0].text, 'Continue this same session');
  const socket = await client;
  socket.send(JSON.stringify({ id: 'approval-rpc', method: 'item/commandExecution/requestApproval', params: {
    threadId: id, turnId: 'turn', itemId: 'command', command: 'npm test', reason: 'Run tests', availableDecisions: ['accept', 'decline'],
  } }));
  const pending = await adapter.read(id);
  assert(pending.ok);
  assert.equal(pending.value.approvals.length, 1);
  assert.equal(state.decision, '');
  const approval = pending.value.approvals[0];
  assert.equal((await adapter.approve(randomUUID(), approval.id, 'accept')).ok, false);
  assert((await adapter.approve(id, approval.id, 'accept')).ok);
  const resolved = await adapter.read(id);
  assert(resolved.ok);
  assert.equal(resolved.value.approvals.length, 0);
  assert.equal(state.decision, 'accept');
  assert.equal((await adapter.approve(id, approval.id, 'accept')).ok, false);
  socket.send(JSON.stringify({ id: 'missing-diff', method: 'item/fileChange/requestApproval', params: {
    threadId: id, turnId: 'turn', itemId: 'unknown-change', reason: 'Change a file',
  } }));
  const missing = await adapter.read(id);
  assert(missing.ok);
  assert.equal(missing.value.approvals[0].canAccept, false);
  assert.equal((await adapter.approve(id, missing.value.approvals[0].id, 'accept')).ok, false);
  assert((await adapter.approve(id, missing.value.approvals[0].id, 'decline')).ok);
  const diff = '-old value\n+new value';
  socket.send(JSON.stringify({ method: 'item/started', params: { threadId: id,
    item: { id: 'reviewable-change', type: 'fileChange', changes: [{ path: '/workspace/file', diff }] },
  } }));
  socket.send(JSON.stringify({ id: 'file-approval', method: 'item/fileChange/requestApproval', params: {
    threadId: id, turnId: 'turn', itemId: 'reviewable-change', reason: 'Apply this change',
  } }));
  const reviewable = await adapter.read(id);
  assert(reviewable.ok);
  assert.equal(reviewable.value.approvals[0].canAccept, true);
  assert(reviewable.value.approvals[0].text.includes(diff));
  assert((await adapter.approve(id, reviewable.value.approvals[0].id, 'accept')).ok);
  state.provider = 'different-provider';
  assert.equal((await adapter.send(id, 'Must not reach this provider')).ok, false);
  assert.equal(state.turns, 2);
  state.endpoint = 'https://another.example.com/v1';
  state.provider = 'gateway';
  assert.equal((await adapter.send(id, 'Do not bypass the gateway')).ok, false);
  assert.equal(state.turns, 2);
  assert.equal((await adapter.create(directory)).ok, false);
  assert.equal(state.starts, 1);
  assert.equal((await adapter.create('relative/path')).ok, false);
  const destination = { gateway: 'https://gateway.example.com' };
  const relay = createServer((_request, response) => {
    response.writeHead(200, { 'Content-Type': 'application/json' });
    response.end(JSON.stringify({ runtime: 'rust', gateway_url: destination.gateway, account: { gateway: { url: destination.gateway } } }));
  });
  await new Promise<void>(resolve => relay.listen(0, '127.0.0.1', resolve));
  t.after(() => { relay.closeAllConnections(); relay.close(); });
  const relayAddress = relay.address();
  assert(relayAddress && typeof relayAddress !== 'string');
  state.endpoint = `http://127.0.0.1:${relayAddress.port}/v1`;
  assert((await adapter.create(directory)).ok);
  assert.equal(state.starts, 2);
  assert((await adapter.interrupt(id)).ok);
  assert.equal(state.interrupts, 1);
  destination.gateway = 'https://different-gateway.example.com';
  assert.equal((await adapter.interrupt(id)).ok, false);
  assert.equal(state.interrupts, 1);
  assert.equal((await adapter.create(directory)).ok, false);
  assert.equal(state.starts, 2);
});
