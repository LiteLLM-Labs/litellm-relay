import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir, rm, readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { Authentication } from './auth.ts';
import type { Gateway, LoginFlow, LoginPoll } from './gateway.ts';
import type { Result } from './protocol.ts';

class TestGateway implements Gateway {
  subject = 'account-one';
  valid = true;
  async start(): Promise<Result<LoginFlow>> {
    return { ok: true, value: { login_id: 'login', poll_secret: 'poll-secret', user_code: 'PAIR-CODE', expires_in: 600 } };
  }
  async poll(): Promise<Result<LoginPoll>> {
    return { ok: true, value: { status: 'ready', key: 'gateway-session-credential', user_id: this.subject } };
  }
  async verify(key: string): Promise<boolean> { return this.valid && key === 'owner-key'; }
}

async function fixture() {
  const directory = resolve('.local/tests', randomUUID());
  await mkdir(directory, { recursive: true });
  return { directory, file: resolve(directory, 'owner.json') };
}

test('only the locally paired LiteLLM account can sign in from another device, including after restart', async t => {
  const files = await fixture();
  t.after(() => rm(files.directory, { recursive: true, force: true }));
  const gateway = new TestGateway();
  const auth = new Authentication(gateway, 'owner-key', files.file);
  assert.equal((await auth.start(false)).ok, false);
  const pairing = await auth.start(true);
  assert(pairing.ok);
  const paired = await auth.poll(pairing.value.token);
  assert(paired.ok && paired.value.status === 'ready');
  assert.equal(auth.identity(paired.value.token)?.subject, gateway.subject);
  assert.equal((await auth.poll(pairing.value.token)).ok, false);
  const stored = await readFile(files.file, 'utf8');
  assert.equal(stored.includes('gateway-session-credential'), false);
  assert.equal(stored.includes('poll-secret'), false);
  const restarted = new Authentication(gateway, 'owner-key', files.file);
  await restarted.load();
  gateway.subject = 'another-account';
  const other = await restarted.start(false);
  assert(other.ok);
  const refused = await restarted.poll(other.value.token);
  assert(!refused.ok && refused.status === 403);
  gateway.subject = 'account-one';
  const returning = await restarted.start(false);
  assert(returning.ok);
  const accepted = await restarted.poll(returning.value.token);
  assert(accepted.ok && accepted.value.status === 'ready');
});

test('local gateway sign-in checks the live credential and cannot be invoked remotely', async t => {
  const files = await fixture();
  t.after(() => rm(files.directory, { recursive: true, force: true }));
  const gateway = new TestGateway();
  const auth = new Authentication(gateway, 'owner-key', files.file);
  assert.equal((await auth.local(false)).ok, false);
  gateway.valid = false;
  assert.equal((await auth.local(true)).ok, false);
  gateway.valid = true;
  const accepted = await auth.local(true);
  assert(accepted.ok);
  assert(auth.identity(accepted.value.token));
  auth.logout(accepted.value.token);
  assert.equal(auth.identity(accepted.value.token), null);
});

test('expired sign-ins and client sessions cannot access the device', async t => {
  const files = await fixture();
  t.after(() => rm(files.directory, { recursive: true, force: true }));
  const clock = { time: 0 };
  const auth = new Authentication(new TestGateway(), 'owner-key', files.file, () => clock.time);
  const flow = await auth.start(true);
  const session = await auth.local(true);
  assert(flow.ok && session.ok);
  clock.time = 601000;
  assert.equal((await auth.poll(flow.value.token)).ok, false);
  assert(auth.identity(session.value.token));
  clock.time = 8 * 3600000 + 1;
  assert.equal(auth.identity(session.value.token), null);
});
