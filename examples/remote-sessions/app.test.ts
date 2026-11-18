import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { z } from 'zod';
import { createApp } from './app.ts';
import { Authentication } from './auth.ts';
import { StateStore } from './state.ts';
import { connectDevice } from './connector.ts';
import type { Sessions } from './protocol.ts';
import type { Gateway } from './gateway.ts';
import { sessionSummary } from './codex-protocol.ts';

test('native clients discover only their devices and control Codex over an authenticated outbound connection', async t => {
  const state = new StateStore(':memory:');
  const gateway: Gateway = {
    start: async () => ({ ok: true, value: { login_id: 'login', poll_secret: 'secret', user_code: 'ABCD', expires_in: 600 } }),
    poll: async () => ({ ok: true, value: { status: 'pending' } }),
    models: async () => ({ ok: true, value: ['test-model'] }),
  };
  const { server, devices } = createApp({ state, auth: new Authentication(gateway, state, 'https://gateway.example.com'),
    gatewayUrl: 'https://gateway.example.com', publicUrl: 'http://127.0.0.1' });
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  assert(address && typeof address !== 'string');
  const endpoint = `http://127.0.0.1:${address.port}`;
  const owner = state.session('owner', ['test-model']);
  const stranger = state.session('stranger');
  const headers = { Authorization: `Bearer ${owner.token}`, 'X-LiteLLM-Client': 'ios', 'Content-Type': 'application/json' };
  const enrolled = await fetch(`${endpoint}/api/devices`, { method: 'POST', headers, body: JSON.stringify({ name: 'Development Mac' }) });
  assert.equal(enrolled.status, 200);
  const device = z.object({ deviceId: z.string(), deviceToken: z.string() }).parse(await enrolled.json());
  const route = `${endpoint}/api/devices/${device.deviceId}/sessions`;
  assert.equal((await fetch(route, { headers: { 'X-LiteLLM-Client': 'ios' } })).status, 401);
  assert.equal((await fetch(route, { headers: { ...headers, Authorization: `Bearer ${stranger.token}` } })).status, 404);
  assert.equal((await fetch(route, { headers })).status, 503);
  const list = z.object({ devices: z.array(z.object({ id: z.string(), online: z.boolean() })) });
  const hidden = list.parse(await (await fetch(`${endpoint}/api/devices`, { headers: { ...headers, Authorization: `Bearer ${stranger.token}` } })).json());
  assert.equal(hidden.devices.length, 0);
  const session = sessionSummary({ id: `codex:${randomUUID()}`, cwd: '/workspace/project', name: 'A task', preview: '', modelProvider: 'gateway',
    createdAt: 1, updatedAt: 2, status: { type: 'idle' } }, true);
  const progress = { text: '', sends: 0, creates: 0, model: '' };
  const sessions: Sessions = {
    list: async () => ({ ok: true, value: { sessions: [session], nextCursor: null } }),
    read: async () => ({ ok: true, value: { session, messages: [{ id: 'message', role: 'user', text: progress.text }], olderMessages: false, approvals: [] } }),
    create: async () => { progress.creates += 1; return { ok: true, value: session }; },
    models: async () => ({ ok: true, value: { models: [{ id: 'test-model', label: 'Test model', isDefault: true },
      { id: 'not-on-gateway', label: 'Unavailable here', isDefault: false }], nextCursor: null } }),
    send: async (_id, text, model) => { progress.sends += 1; progress.text = text; progress.model = model ?? ''; return { ok: true, value: { turnId: 'turn' } }; },
    approve: async () => ({ ok: true, value: { resolved: true } }),
    interrupt: async () => ({ ok: true, value: { stopped: true } }),
  };
  let connected: () => void = () => {};
  const opened = new Promise<void>(resolve => { connected = resolve; });
  const connector = connectDevice({ endpoint, token: device.deviceToken, sessions, onStatus: status => { if (status.startsWith('Connected')) connected(); } });
  t.after(() => { connector.stop(); devices.close(); server.closeAllConnections(); server.close(); state.close(); });
  await opened;
  const online = list.parse(await (await fetch(`${endpoint}/api/devices`, { headers })).json());
  assert.equal(online.devices[0].online, true);
  const threads = z.object({ sessions: z.array(z.object({ id: z.string() })) }).parse(await (await fetch(route, { headers })).json());
  assert.equal(threads.sessions[0].id, session.id);
  const input = { requestId: randomUUID(), text: 'Continue from the phone', model: 'test-model' };
  const send = (inputHeaders: Readonly<Record<string, string>> = headers) => fetch(`${route}/${session.id}/messages`, { method: 'POST', headers: inputHeaders, body: JSON.stringify(input) });
  assert.equal((await send()).status, 200);
  assert.equal((await send()).status, 200);
  assert.equal(progress.sends, 1);
  assert.equal(progress.text, input.text);
  assert.equal(progress.model, input.model);
  const catalog = z.object({ models: z.array(z.object({ id: z.string() })) }).parse(await (await fetch(`${endpoint}/api/devices/${device.deviceId}/models`, { headers })).json());
  assert.equal(catalog.models[0].id, input.model);
  assert.equal(catalog.models.length, 1);
  assert.equal((await fetch(`${route}/${session.id}/messages`, { method: 'POST', headers,
    body: JSON.stringify({ requestId: randomUUID(), text: 'Do not forward this model', model: 'not-on-gateway' }) })).status, 403);
  assert.equal(progress.sends, 1);
  assert.equal((await send({ ...headers, Origin: 'https://another.example' })).status, 403);
  assert.equal((await fetch(`${route}/${session.id}/messages`, { method: 'POST', headers, body: JSON.stringify({ requestId: randomUUID(), text: '' }) })).status, 400);
  assert.equal((await fetch(`${route}/${session.id}/messages`, { method: 'POST', headers, body: JSON.stringify({ ...input, text: 'different' }) })).status, 409);
  const create = () => fetch(route, { method: 'POST', headers: { ...headers, 'X-LiteLLM-Client': 'android' }, body: JSON.stringify({ requestId: input.requestId, cwd: '/workspace/project', agent: 'codex' }) });
  assert.equal((await create()).status, 409);
  const created = await fetch(route, { method: 'POST', headers, body: JSON.stringify({ requestId: randomUUID(), cwd: '/workspace/project', agent: 'codex' }) });
  assert.equal(created.status, 200);
  assert.equal(progress.creates, 1);
  assert.equal((await fetch(`${endpoint}/api/devices/${device.deviceId}`, { method: 'DELETE', headers })).status, 200);
  assert.equal((await send()).status, 404);
  assert.equal(state.authenticateDevice(device.deviceToken), null);
  assert.equal((await fetch(`${endpoint}/api/auth/logout`, { method: 'POST', headers, body: '{}' })).status, 200);
  assert.equal((await fetch(`${endpoint}/api/devices`, { headers })).status, 401);
});
