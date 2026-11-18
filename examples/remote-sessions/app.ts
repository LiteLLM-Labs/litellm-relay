import { createServer } from 'node:http';
import type { IncomingMessage, ServerResponse } from 'node:http';
import type { Authentication } from './auth.ts';
import type { Sessions } from './codex.ts';
import type { Result } from './protocol.ts';

export type AppOptions = Readonly<{
  auth: Authentication;
  sessions: Sessions;
  gatewayUrl: string;
  machineName: string;
  allowedHosts: ReadonlySet<string>;
  phoneUrls: readonly string[];
  secureCookies?: boolean;
}>;

function cookie(request: IncomingMessage, name: string): string | undefined {
  return request.headers.cookie?.split(';').map(value => value.trim()).find(value => value.startsWith(`${name}=`))?.slice(name.length + 1);
}

function setCookie(response: ServerResponse, name: string, value: string, seconds: number, secure: boolean): void {
  response.appendHeader('Set-Cookie', `${name}=${value}; Path=/api; HttpOnly; SameSite=Lax; Max-Age=${seconds}${secure ? '; Secure' : ''}`);
}

function json(response: ServerResponse, status: number, value: unknown): void {
  response.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8' });
  response.end(JSON.stringify(value));
}

function result<T>(response: ServerResponse, value: Result<T>): void {
  if (value.ok) json(response, 200, value.value);
  else json(response, value.status, { error: value.error });
}

export function createApp(options: AppOptions) {
  const attempts = new Map<string, Readonly<{ count: number; until: number }>>();
  const server = createServer((request, response) => {
    response.setHeader('Cache-Control', 'no-store');
    response.setHeader('X-Content-Type-Options', 'nosniff');
    response.setHeader('Referrer-Policy', 'no-referrer');
    response.setHeader('Content-Security-Policy', "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'");
    void dispatch(request, response).catch(() => {
      if (!response.headersSent) json(response, 500, { error: 'The request could not be completed.' });
      else response.end();
    });
  });
  server.requestTimeout = 15000;
  server.headersTimeout = 10000;

  async function dispatch(request: IncomingMessage, response: ServerResponse): Promise<void> {
    const host = request.headers.host ?? '';
    const protocol = options.secureCookies ? 'https' : 'http';
    const url = new URL(request.url ?? '/', `${protocol}://${host || 'invalid'}`);
    if (!options.allowedHosts.has(url.hostname)) return json(response, 403, { error: 'Host not allowed.' });
    const origin = request.headers.origin;
    if ((origin && origin !== url.origin) || request.headers['sec-fetch-site'] === 'cross-site') {
      return json(response, 403, { error: 'Cross-site requests are not allowed.' });
    }
    const nativeClient = request.headers['x-litellm-client'] === 'ios' && !origin && !request.headers['sec-fetch-site'];
    if (request.method === 'POST' && origin !== url.origin && !nativeClient) return json(response, 403, { error: 'A same-origin request or native client is required.' });
    const local = ['127.0.0.1', '::1', '::ffff:127.0.0.1'].includes(request.socket.remoteAddress ?? '');
    const token = cookie(request, 'relay_session');
    const identity = options.auth.identity(token);

    if (url.pathname === '/' && request.method === 'GET') return json(response, 200, { service: 'LiteLLM Remote bridge', client: 'iOS' });
    if (url.pathname === '/api/me' && request.method === 'GET') return json(response, 200, {
      identity, gateway: options.gatewayUrl, paired: options.auth.paired,
      localConnection: local && options.auth.hasLocalConnection,
      canPair: local,
    });
    if (url.pathname.startsWith('/api/auth/') && request.method === 'POST') {
      const address = request.socket.remoteAddress ?? 'unknown';
      for (const [ip, value] of attempts) if (value.until < Date.now()) attempts.delete(ip);
      const entry = attempts.get(address);
      if ((entry?.count ?? 0) >= 20 || attempts.size > 1024) return json(response, 429, { error: 'Too many attempts. Please wait a minute.' });
      attempts.set(address, { count: (entry?.count ?? 0) + 1, until: entry?.until ?? Date.now() + 60000 });
    }
    if (url.pathname === '/api/auth/start' && request.method === 'POST') {
      const started = await options.auth.start(local);
      if (!started.ok) return result(response, started);
      setCookie(response, 'relay_flow', started.value.token, 600, Boolean(options.secureCookies));
      return json(response, 200, {
        code: started.value.code, url: '/api/auth/authorize',
        verificationUrl: nativeClient ? options.auth.verificationUrl(started.value.token, options.gatewayUrl) : undefined,
      });
    }
    if (url.pathname === '/api/auth/poll' && request.method === 'GET') {
      if (identity && !cookie(request, 'relay_flow')) return json(response, 200, { status: 'ready', identity });
      const polled = await options.auth.poll(cookie(request, 'relay_flow'), url.searchParams.get('team') ?? undefined);
      if (!polled.ok) return result(response, polled);
      if (polled.value.status !== 'ready') return json(response, 200, polled.value);
      setCookie(response, 'relay_session', polled.value.token, 28800, Boolean(options.secureCookies));
      setCookie(response, 'relay_flow', '', 0, Boolean(options.secureCookies));
      return json(response, 200, { status: 'ready', identity: polled.value.identity });
    }
    if (url.pathname === '/api/auth/local' && request.method === 'POST') {
      const signedIn = await options.auth.local(local);
      if (!signedIn.ok) return result(response, signedIn);
      setCookie(response, 'relay_session', signedIn.value.token, 28800, Boolean(options.secureCookies));
      return json(response, 200, { identity: signedIn.value.identity });
    }
    if (url.pathname === '/api/auth/logout' && request.method === 'POST') {
      options.auth.logout(token);
      setCookie(response, 'relay_session', '', 0, Boolean(options.secureCookies));
      return json(response, 200, { signedOut: true });
    }
    if (!identity) return json(response, 401, { error: 'Sign in to view your sessions.' });
    if (url.pathname === '/api/device' && request.method === 'GET') return json(response, 200, {
      name: options.machineName, gateway: options.gatewayUrl, phoneUrls: options.phoneUrls, paired: options.auth.paired,
      mode: 'read-only',
    });
    if (url.pathname === '/api/sessions' && request.method === 'GET') {
      return result(response, await options.sessions.list(url.searchParams.get('cursor') ?? undefined));
    }
    const match = /^\/api\/sessions\/([a-f0-9-]+)$/.exec(url.pathname);
    if (match && request.method === 'GET') return result(response, await options.sessions.read(match[1]));
    return json(response, 404, { error: 'Not found.' });
  }
  return server;
}
