import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { rm } from 'node:fs/promises';
import { resolve } from 'node:path';
import { createApp } from './app.ts';
import { Authentication } from './auth.ts';
import type { Gateway } from './gateway.ts';
import type { Sessions } from './codex.ts';
import { sessionSummary } from './protocol.ts';
import { z } from 'zod';

test('native login gates session reads, refreshes content, and revokes access on logout', async t => {
  const directory = resolve('.local/tests', randomUUID());
  t.after(() => rm(directory, { recursive: true, force: true }));
  const gateway: Gateway = {
    start: async () => ({ ok: true, value: { login_id: 'login', poll_secret: 'private-poll-secret', user_code: 'USER-CODE', expires_in: 600 } }),
    poll: async () => ({ ok: true, value: { status: 'ready', key: 'private-gateway-token', user_id: 'owner' } }),
    verify: async key => key === 'owner-key',
  };
  const session = sessionSummary({ id: randomUUID(), cwd: '/workspace/project', name: 'A running task', preview: '',
    modelProvider: 'gateway', createdAt: 1, updatedAt: 2, status: { type: 'active' } }, true);
  const progress = { text: 'First update' };
  const sessions: Sessions = {
    list: async () => ({ ok: true, value: { sessions: [session], nextCursor: null } }),
    read: async () => ({ ok: true, value: { session, messages: [{ id: 'message', role: 'assistant', text: progress.text }], olderMessages: false } }),
  };
  const auth = new Authentication(gateway, 'owner-key', resolve(directory, 'owner.json'));
  const server = createApp({ auth, sessions, gatewayUrl: 'https://gateway.example.com', machineName: 'Test Mac',
    allowedHosts: new Set(['127.0.0.1']), phoneUrls: [] });
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => { server.closeAllConnections(); server.close(); });
  const address = server.address();
  assert(address && typeof address !== 'string');
  const base = `http://127.0.0.1:${address.port}`;
  assert.equal((await fetch(`${base}/api/sessions`)).status, 401);
  assert.equal((await fetch(`${base}/api/sessions/${session.id}`)).status, 401);
  const crossSite = await fetch(`${base}/api/auth/local`, { method: 'POST', headers: { Origin: 'https://another.example', 'X-LiteLLM-Client': 'ios' } });
  assert.equal(crossSite.status, 403);
  assert.equal((await fetch(`${base}/api/auth/local`, { method: 'POST' })).status, 403);
  const signedIn = await fetch(`${base}/api/auth/local`, { method: 'POST', headers: { 'X-LiteLLM-Client': 'ios' } });
  assert.equal(signedIn.status, 200);
  const cookie = signedIn.headers.get('set-cookie')?.split(';')[0];
  assert(cookie);
  const headers = { Cookie: cookie, Origin: base };
  const listed = await fetch(`${base}/api/sessions`, { headers });
  const list = z.object({ sessions: z.array(z.object({ id: z.string() })) }).parse(await listed.json());
  assert.equal(list.sessions[0].id, session.id);
  assert((await (await fetch(`${base}/api/sessions/${session.id}`, { headers })).text()).includes(progress.text));
  progress.text = 'New live update';
  assert((await (await fetch(`${base}/api/sessions/${session.id}`, { headers })).text()).includes(progress.text));
  assert.equal((await fetch(`${base}/api/sessions/${session.id}/message`, { method: 'POST', headers })).status, 404);
  const start = await fetch(`${base}/api/auth/start`, { method: 'POST', headers });
  const publicFlow = await start.text();
  assert(publicFlow.includes('USER-CODE'));
  assert.equal(publicFlow.includes('private-poll-secret'), false);
  assert.equal(publicFlow.includes('owner-key'), false);
  const nativeStart = await fetch(`${base}/api/auth/start`, { method: 'POST', headers: { 'X-LiteLLM-Client': 'ios' } });
  const nativeFlow = z.object({ code: z.string(), verificationUrl: z.string() }).parse(await nativeStart.json());
  assert.equal(new URL(nativeFlow.verificationUrl).origin, 'https://gateway.example.com');
  assert.equal((await fetch(`${base}/api/auth/logout`, { method: 'POST', headers })).status, 200);
  assert.equal((await fetch(`${base}/api/sessions`, { headers })).status, 401);
});
