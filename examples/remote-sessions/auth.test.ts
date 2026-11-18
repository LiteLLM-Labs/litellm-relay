import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir, rm, readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { z } from 'zod';
import { Authentication } from './auth.ts';
import { StateStore } from './state.ts';
import type { Gateway } from './gateway.ts';

test('SSO authenticates separate accounts without exposing gateway keys and consumes each sign-in once', async t => {
  const state = new StateStore(':memory:');
  t.after(() => state.close());
  const account = { subject: 'one' };
  const gateway: Gateway = {
    start: async () => ({ ok: true, value: { login_id: 'login', poll_secret: 'private-poll-secret', user_code: 'ABCD', expires_in: 600 } }),
    poll: async () => ({ ok: true, value: { status: 'ready', user_id: account.subject, key: 'private-inference-key' } }),
    models: async key => { assert.equal(key, 'private-inference-key'); return { ok: true, value: [`${account.subject}-model`] }; },
  };
  const auth = new Authentication(gateway, state, 'https://gateway.example.com');
  const first = await auth.start();
  assert(first.ok);
  const firstReply = await auth.poll(first.value.flowToken);
  assert(firstReply.ok);
  const firstSession = z.object({ token: z.string() }).parse(firstReply.value);
  assert.equal(state.identity(firstSession.token)?.subject, 'one');
  assert.deepEqual(state.models(firstSession.token), ['one-model']);
  assert.equal((await auth.poll(first.value.flowToken)).ok, false);
  account.subject = 'two';
  const second = await auth.start();
  assert(second.ok);
  const secondReply = await auth.poll(second.value.flowToken);
  assert(secondReply.ok);
  const secondSession = z.object({ token: z.string() }).parse(secondReply.value);
  assert.equal(state.identity(secondSession.token)?.subject, 'two');
  assert.deepEqual(state.models(secondSession.token), ['two-model']);
  assert.notEqual(firstSession.token, secondSession.token);
  assert.equal(JSON.stringify(firstReply).includes('private-inference-key'), false);
  assert.equal(JSON.stringify(first).includes('private-poll-secret'), false);
  state.logout(firstSession.token);
  assert.equal(state.identity(firstSession.token), null);
  assert(state.identity(secondSession.token));
});

test('device ownership, revocation, session expiry and duplicate-write protection survive service restart', async t => {
  const directory = resolve('.local/tests', randomUUID());
  await mkdir(directory, { recursive: true });
  t.after(() => rm(directory, { recursive: true, force: true }));
  const path = resolve(directory, 'state.sqlite');
  const clock = { now: 0 };
  const state = new StateStore(path, () => clock.now);
  const session = state.session('owner', ['gateway-model']);
  const enrolled = state.enroll('owner', 'My Mac');
  assert.equal(state.device(enrolled.device.id, 'other'), null);
  const requestId = randomUUID();
  assert.deepEqual(state.begin('owner', requestId, 'fingerprint'), { ok: true, value: null });
  state.finish('owner', requestId, { ok: true, value: { turnId: 'accepted-turn' } });
  const pendingId = randomUUID();
  assert.deepEqual(state.begin('owner', pendingId, 'uncertain'), { ok: true, value: null });
  state.close();
  const restarted = new StateStore(path, () => clock.now);
  t.after(() => restarted.close());
  assert.equal(restarted.identity(session.token)?.subject, 'owner');
  assert.deepEqual(restarted.models(session.token), ['gateway-model']);
  assert.equal(restarted.authenticateDevice(enrolled.deviceToken)?.id, enrolled.device.id);
  const duplicate = restarted.begin('owner', requestId, 'fingerprint');
  assert(duplicate.ok && duplicate.value?.response?.ok);
  assert.deepEqual(duplicate.value.response.value, { turnId: 'accepted-turn' });
  const uncertain = restarted.begin('owner', pendingId, 'uncertain');
  assert(uncertain.ok && uncertain.value && uncertain.value.response === null);
  assert.equal(restarted.begin('owner', requestId, 'different').ok, false);
  assert.deepEqual(restarted.begin('another-owner', requestId, 'fingerprint'), { ok: true, value: null });
  const bytes = await readFile(path);
  assert.equal(bytes.includes(session.token), false);
  assert.equal(bytes.includes(enrolled.deviceToken), false);
  restarted.revoke(enrolled.device.id, 'owner');
  assert.equal(restarted.authenticateDevice(enrolled.deviceToken), null);
  clock.now = session.identity.expiresAt + 1;
  assert.equal(restarted.identity(session.token), null);
});
