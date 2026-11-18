import WebSocket from 'ws';
import type { Sessions } from './protocol.ts';
import { commandSchema, dispatchAction } from './control.ts';
import { failure } from './protocol.ts';

export function connectDevice(options: Readonly<{
  endpoint: string; token: string; sessions: Sessions; onStatus?: (status: string) => void;
}>) {
  let stopped = false;
  let socket: WebSocket | null = null;
  let retry: ReturnType<typeof setTimeout> | undefined;
  let failures = 0;
  const active = new Set<string>();

  function connect(): void {
    if (stopped) return;
    const url = new URL('/agent', options.endpoint);
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
    const connection = new WebSocket(url, { headers: { Authorization: `Bearer ${options.token}` },
      maxPayload: 131072, handshakeTimeout: 10000, followRedirects: false });
    socket = connection;
    connection.on('open', () => { failures = 0; options.onStatus?.('Connected to LiteLLM Remote'); });
    connection.on('message', raw => {
      try {
        const command = commandSchema.safeParse(JSON.parse(raw.toString()));
        if (!command.success) { connection.close(1008, 'Invalid command'); return; }
        const { id, action } = command.data;
        if (active.has(id)) return;
        if (active.size >= 16) { connection.send(JSON.stringify({ id, result: failure(429, 'This device is busy.') })); return; }
        active.add(id);
        void dispatchAction(options.sessions, action).catch(() => failure(502, 'The coding agent could not complete this request.'))
          .then(result => {
            if (connection.readyState === WebSocket.OPEN) connection.send(JSON.stringify({ id, result }));
          }).finally(() => active.delete(id));
      } catch { connection.close(1008, 'Invalid command'); }
    });
    connection.on('error', () => {});
    connection.on('unexpected-response', (_request, response) => {
      response.resume();
      connection.terminate();
      if ([401, 403].includes(response.statusCode ?? 0)) {
        stopped = true;
        options.onStatus?.('Device access was revoked. Enroll this Mac again to reconnect.');
      }
    });
    connection.on('close', (code, reason) => {
      if (code === 1008 || reason.toString() === 'Reconnected') stopped = true;
      if (stopped) return;
      failures += 1;
      options.onStatus?.('Reconnecting to LiteLLM Remote');
      retry = setTimeout(connect, Math.min(30000, 1000 * 2 ** Math.min(failures, 5)) + Math.random() * 1000);
    });
  }

  connect();
  return { stop(): void { stopped = true; clearTimeout(retry); socket?.close(); } };
}
