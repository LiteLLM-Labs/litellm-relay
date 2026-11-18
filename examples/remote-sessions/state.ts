import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { DatabaseSync } from 'node:sqlite';
import { z } from 'zod';
import type { Identity, Result } from './protocol.ts';
import { failure } from './protocol.ts';

const digest = (token: string) => createHash('sha256').update(token).digest('hex');
const accountSchema = z.object({ subject: z.string(), label: z.string(), expires_at: z.number() });
const deviceSchema = z.object({ id: z.string(), subject: z.string(), name: z.string(), last_seen: z.number() });
export type Device = Readonly<{ id: string; subject: string; name: string; last_seen: number }>;
export type CachedRequest = Readonly<{ fingerprint: string; response: Result<unknown> | null }>;
const resultSchema = z.discriminatedUnion('ok', [
  z.object({ ok: z.literal(true), value: z.unknown() }),
  z.object({ ok: z.literal(false), status: z.number(), error: z.string() }),
]);

export class StateStore {
  private readonly db: DatabaseSync;
  private readonly now: () => number;

  constructor(path: string, now: () => number = Date.now) {
    this.now = now;
    this.db = new DatabaseSync(path);
    this.db.exec(`
      PRAGMA journal_mode=WAL;
      CREATE TABLE IF NOT EXISTS sessions (token_hash TEXT PRIMARY KEY, subject TEXT NOT NULL, label TEXT NOT NULL, expires_at INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS catalogs (token_hash TEXT PRIMARY KEY, models TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS devices (id TEXT PRIMARY KEY, subject TEXT NOT NULL, name TEXT NOT NULL, token_hash TEXT NOT NULL UNIQUE, last_seen INTEGER NOT NULL);
      CREATE INDEX IF NOT EXISTS devices_subject ON devices(subject);
      CREATE TABLE IF NOT EXISTS requests (subject TEXT NOT NULL, request_id TEXT NOT NULL, fingerprint TEXT NOT NULL, response TEXT, expires_at INTEGER NOT NULL, PRIMARY KEY(subject, request_id));
    `);
  }

  session(subject: string, models: readonly string[] = []): Readonly<{ token: string; identity: Identity }> {
    const token = randomBytes(32).toString('base64url');
    const identity: Identity = { subject, label: subject, method: 'sso', expiresAt: this.now() + 7 * 86400000 };
    this.db.prepare('DELETE FROM sessions WHERE expires_at <= ?').run(this.now());
    this.db.prepare('DELETE FROM catalogs WHERE token_hash NOT IN (SELECT token_hash FROM sessions)').run();
    this.db.prepare('INSERT INTO sessions VALUES (?, ?, ?, ?)').run(digest(token), subject, subject, identity.expiresAt);
    this.db.prepare('INSERT INTO catalogs VALUES (?, ?)').run(digest(token), JSON.stringify(models));
    return { token, identity };
  }

  identity(token: string | undefined): Identity | null {
    if (!token) return null;
    const parsed = accountSchema.safeParse(this.db.prepare('SELECT subject, label, expires_at FROM sessions WHERE token_hash = ? AND expires_at > ?').get(digest(token), this.now()));
    return parsed.success ? { subject: parsed.data.subject, label: parsed.data.label, method: 'sso', expiresAt: parsed.data.expires_at } : null;
  }

  logout(token: string): void {
    this.db.prepare('DELETE FROM sessions WHERE token_hash = ?').run(digest(token));
    this.db.prepare('DELETE FROM catalogs WHERE token_hash = ?').run(digest(token));
  }

  models(token: string): readonly string[] {
    const row = z.object({ models: z.string() }).safeParse(this.db.prepare('SELECT models FROM catalogs WHERE token_hash = ?').get(digest(token)));
    return row.success ? z.array(z.string()).parse(JSON.parse(row.data.models)) : [];
  }

  enroll(subject: string, name: string): Readonly<{ device: Device; deviceToken: string }> {
    const deviceToken = randomBytes(32).toString('base64url');
    const device = { id: randomUUID(), subject, name, last_seen: this.now() };
    this.db.prepare('INSERT INTO devices VALUES (?, ?, ?, ?, ?)').run(device.id, subject, name, digest(deviceToken), device.last_seen);
    return { device, deviceToken };
  }

  devices(subject: string): readonly Device[] {
    return z.array(deviceSchema).parse(this.db.prepare('SELECT id, subject, name, last_seen FROM devices WHERE subject = ? ORDER BY name').all(subject));
  }

  device(id: string, subject: string): Device | null {
    return this.devices(subject).find(device => device.id === id) ?? null;
  }

  authenticateDevice(token: string): Device | null {
    const parsed = deviceSchema.safeParse(this.db.prepare('SELECT id, subject, name, last_seen FROM devices WHERE token_hash = ?').get(digest(token)));
    return parsed.success ? parsed.data : null;
  }

  seen(id: string): void { this.db.prepare('UPDATE devices SET last_seen = ? WHERE id = ?').run(this.now(), id); }
  revoke(id: string, subject: string): void { this.db.prepare('DELETE FROM devices WHERE id = ? AND subject = ?').run(id, subject); }

  begin(subject: string, requestId: string, fingerprint: string): Result<CachedRequest | null> {
    this.db.prepare('DELETE FROM requests WHERE expires_at <= ?').run(this.now());
    const previous = z.object({ fingerprint: z.string(), response: z.string().nullable() }).safeParse(
      this.db.prepare('SELECT fingerprint, response FROM requests WHERE subject = ? AND request_id = ?').get(subject, requestId));
    if (previous.success) {
      if (previous.data.fingerprint !== fingerprint) return failure(409, 'This request ID was already used.');
      return { ok: true, value: { fingerprint, response: previous.data.response ? resultSchema.parse(JSON.parse(previous.data.response)) : null } };
    }
    this.db.prepare('INSERT INTO requests VALUES (?, ?, ?, NULL, ?)').run(subject, requestId, fingerprint, this.now() + 86400000);
    return { ok: true, value: null };
  }

  finish(subject: string, requestId: string, response: Result<unknown>): void {
    this.db.prepare('UPDATE requests SET response = ? WHERE subject = ? AND request_id = ?').run(JSON.stringify(response), subject, requestId);
  }

  close(): void { this.db.close(); }
}
