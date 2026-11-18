import { createServer } from 'node:http';
import type { IncomingMessage, ServerResponse } from 'node:http';
import { createHash } from 'node:crypto';
import { z } from 'zod';
import type { Authentication } from './auth.ts';
import type { StateStore } from './state.ts';
import type { Action } from './control.ts';
import { DeviceConnections } from './control.ts';
import { approvalInput, failure, messageInput, newSessionInput, sessionIdSchema } from './protocol.ts';
import type { Result } from './protocol.ts';

type Options = Readonly<{ auth: Authentication; state: StateStore; gatewayUrl: string; publicUrl: string }>;

function json(response: ServerResponse, status: number, value: unknown): void {
  response.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store',
    'X-Content-Type-Options': 'nosniff', 'Referrer-Policy': 'no-referrer' });
  response.end(JSON.stringify(value));
}

function reply(response: ServerResponse, result: Result<unknown>): void {
  if (result.ok) json(response, 200, result.value);
  else json(response, result.status, { error: result.error });
}

async function body<T>(request: IncomingMessage, schema: z.ZodType<T>): Promise<Result<T>> {
  if (request.headers['content-type']?.split(';')[0].trim() !== 'application/json') return failure(415, 'Send JSON data.');
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of request.iterator({ destroyOnReturn: false })) {
    const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(String(chunk));
    size += buffer.length;
    if (size > 131072) { request.resume(); return failure(413, 'This request is too large.'); }
    chunks.push(buffer);
  }
  try {
    const parsed = schema.safeParse(JSON.parse(Buffer.concat(chunks).toString('utf8')));
    return parsed.success ? { ok: true, value: parsed.data } : failure(400, 'Check the request and try again.');
  } catch { return failure(400, 'Send valid JSON data.'); }
}

export function createApp(options: Options) {
  const attempts = new Map<string, Readonly<{ count: number; until: number }>>();
  const server = createServer((request, response) => {
    void dispatch(request, response).catch(() => {
      if (!response.headersSent) json(response, 500, { error: 'The request could not be completed.' });
      else response.end();
    });
  });
  const devices = new DeviceConnections(server, options.state);
  server.requestTimeout = 15000;
  server.headersTimeout = 10000;
  server.once('close', () => devices.close());

  async function once<T extends Readonly<{ requestId: string }>>(
    request: IncomingMessage, subject: string, scope: string, schema: z.ZodType<T>, action: (input: T) => Promise<Result<unknown>>,
  ): Promise<Result<unknown>> {
    const input = await body(request, schema);
    if (!input.ok) return input;
    const fingerprint = createHash('sha256').update(JSON.stringify([scope, input.value])).digest('hex');
    const begun = options.state.begin(subject, input.value.requestId, fingerprint);
    if (!begun.ok) return begun;
    if (begun.value) return begun.value.response ?? failure(409, 'This request was already submitted. Refresh the session before sending again.');
    const result = await action(input.value).catch(() => failure(502, 'The request may have reached your Mac. Refresh before sending again.'));
    options.state.finish(subject, input.value.requestId, result);
    return result;
  }

  async function dispatch(request: IncomingMessage, response: ServerResponse): Promise<void> {
    const url = new URL(request.url ?? '/', options.publicUrl);
    if (url.pathname === '/healthz' && request.method === 'GET') return json(response, 200, { status: 'ok' });
    if (url.pathname === '/.well-known/litellm-remote.json' && request.method === 'GET') return json(response, 200,
      { gatewayUrl: options.gatewayUrl, controlUrl: options.publicUrl });
    const host = request.headers.host;
    if (!host || new URL(`http://${host}`).hostname !== new URL(options.publicUrl).hostname) return json(response, 403, { error: 'Host not allowed.' });
    if (request.headers.origin || request.headers['sec-fetch-site']) return json(response, 403, { error: 'Use the LiteLLM Remote app.' });
    if (!['ios', 'android', 'relay'].includes(String(request.headers['x-litellm-client']))) return json(response, 403, { error: 'Use the LiteLLM Remote app.' });
    const token = request.headers.authorization?.match(/^Bearer ([A-Za-z0-9_-]{43})$/)?.[1];
    const identity = options.state.identity(token);
    if (url.pathname === '/api/me' && request.method === 'GET') return json(response, 200, { identity, gateway: options.gatewayUrl });
    if (url.pathname.startsWith('/api/auth/')) {
      const ip = request.socket.remoteAddress ?? 'unknown';
      for (const [address, entry] of attempts) if (entry.until <= Date.now()) attempts.delete(address);
      const previous = attempts.get(ip);
      if ((previous?.count ?? 0) >= 100 || attempts.size >= 1000) return json(response, 429, { error: 'Too many sign-in attempts. Wait a minute.' });
      attempts.set(ip, { count: (previous?.count ?? 0) + 1, until: previous?.until ?? Date.now() + 60000 });
    }
    if (url.pathname === '/api/auth/start' && request.method === 'POST') return reply(response, await options.auth.start());
    if (url.pathname === '/api/auth/poll' && request.method === 'POST') {
      const input = await body(request, z.object({ flowToken: z.string().min(40).max(100), team: z.string().max(500).optional() }).strict());
      return reply(response, input.ok ? await options.auth.poll(input.value.flowToken, input.value.team) : input);
    }
    if (!identity || !token) return json(response, 401, { error: 'Sign in to your LiteLLM workspace.' });
    if (url.pathname === '/api/auth/logout' && request.method === 'POST') {
      options.state.logout(token);
      return json(response, 200, { signedOut: true });
    }
    if (url.pathname === '/api/devices' && request.method === 'GET') return json(response, 200, {
      devices: options.state.devices(identity.subject).map(device => ({ id: device.id, name: device.name,
        online: devices.online(device.id), lastSeen: device.last_seen, platform: 'macOS' })),
    });
    if (url.pathname === '/api/devices' && request.method === 'POST') {
      const input = await body(request, z.object({ name: z.string().trim().min(1).max(100) }).strict());
      if (!input.ok) return reply(response, input);
      if (options.state.devices(identity.subject).length >= 20) return json(response, 429, { error: 'Remove an old device before adding another.' });
      const enrolled = options.state.enroll(identity.subject, input.value.name);
      return json(response, 200, { deviceId: enrolled.device.id, deviceToken: enrolled.deviceToken, gateway: options.gatewayUrl });
    }
    const route = /^\/api\/devices\/([a-f0-9-]+)(?:\/(.*))?$/.exec(url.pathname);
    const device = route ? options.state.device(route[1], identity.subject) : null;
    if (!route || !device) return json(response, 404, { error: 'Device not found.' });
    if (!route[2] && request.method === 'DELETE') {
      options.state.revoke(device.id, identity.subject);
      devices.disconnect(device.id);
      return json(response, 200, { removed: true });
    }
    const operation = route[2] ?? '';
    const run = (action: Action) => devices.request(device, action);
    if (operation === 'models' && request.method === 'GET') {
      const available = options.state.models(token);
      if (!available.length) return json(response, 502, { error: 'Your gateway did not return any models. Sign in again to refresh model access.' });
      const catalog = await run({ type: 'models' });
      const parsed = catalog.ok ? z.object({ models: z.array(z.object({ id: z.string(), label: z.string(), isDefault: z.boolean() })) }).safeParse(catalog.value) : null;
      const labels = parsed?.success ? parsed.data.models : [];
      const models = available.map(id => ({ id, label: labels.find(model => model.id === id)?.label ?? id,
        isDefault: labels.some(model => model.id === id && model.isDefault) }));
      const known = new Set(labels.map(model => model.id));
      return json(response, 200, { models: models.toSorted((a, b) => Number(known.has(b.id)) - Number(known.has(a.id)) || a.label.localeCompare(b.label)), nextCursor: null });
    }
    if (operation === 'sessions' && request.method === 'GET') return reply(response, await run({ type: 'list', cursor: url.searchParams.get('cursor') ?? undefined }));
    if (operation === 'sessions' && request.method === 'POST') return reply(response,
      await once(request, identity.subject, url.pathname, newSessionInput, input => run({ type: 'create', cwd: input.cwd, agent: input.agent })));
    const thread = /^sessions\/([^/]+)(?:\/(.*))?$/.exec(operation);
    if (!thread || !sessionIdSchema.safeParse(thread[1]).success) return json(response, 404, { error: 'Session not found.' });
    if (!thread[2] && request.method === 'GET') return reply(response, await run({ type: 'read', sessionId: thread[1] }));
    if (thread[2] === 'messages' && request.method === 'POST') return reply(response,
      await once(request, identity.subject, url.pathname, messageInput, input => input.model && !options.state.models(token).includes(input.model)
        ? Promise.resolve(failure(403, 'This model is not available to this signed-in account.'))
        : run({ type: 'send', sessionId: thread[1], text: input.text, model: input.model })));
    if (thread[2] === 'interrupt' && request.method === 'POST') return reply(response,
      await once(request, identity.subject, url.pathname, z.object({ requestId: z.string().uuid() }).strict(), () => run({ type: 'interrupt', sessionId: thread[1] })));
    const approval = /^approvals\/([a-f0-9-]+)$/.exec(thread[2] ?? '');
    if (approval && z.string().uuid().safeParse(approval[1]).success && request.method === 'POST') {
      const input = await body(request, approvalInput);
      return reply(response, input.ok ? await run({ type: 'approve', sessionId: thread[1], approvalId: approval[1], decision: input.value.decision }) : input);
    }
    return json(response, 404, { error: 'Not found.' });
  }

  return { server, devices };
}
