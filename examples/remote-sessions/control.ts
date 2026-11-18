import { randomUUID } from 'node:crypto';
import WebSocket, { WebSocketServer } from 'ws';
import { z } from 'zod';
import type { Server, IncomingMessage } from 'node:http';
import type { Duplex } from 'node:stream';
import type { Sessions } from './protocol.ts';
import type { Device, StateStore } from './state.ts';
import type { Result } from './protocol.ts';
import { agentKind, failure, sessionIdSchema } from './protocol.ts';

export const actionSchema = z.discriminatedUnion('type', [
  z.object({ type: z.literal('list'), cursor: z.string().optional() }),
  z.object({ type: z.literal('read'), sessionId: sessionIdSchema }),
  z.object({ type: z.literal('create'), agent: agentKind, cwd: z.string().min(1).max(4096) }),
  z.object({ type: z.literal('interrupt'), sessionId: sessionIdSchema }),
  z.object({ type: z.literal('models'), cursor: z.string().optional() }),
  z.object({ type: z.literal('send'), sessionId: sessionIdSchema, text: z.string().trim().min(1).max(32000), model: z.string().trim().min(1).max(200).optional() }),
  z.object({ type: z.literal('approve'), sessionId: sessionIdSchema, approvalId: z.string().uuid(), decision: z.enum(['accept', 'decline']) }),
]);
export type Action = z.infer<typeof actionSchema>;
export const commandSchema = z.object({ id: z.string().uuid(), action: actionSchema });
const responseSchema = z.object({ id: z.string().uuid(), result: z.discriminatedUnion('ok', [
  z.object({ ok: z.literal(true), value: z.unknown() }),
  z.object({ ok: z.literal(false), status: z.number().int().min(400).max(599), error: z.string().max(1000) }),
]) });

export function dispatchAction(sessions: Sessions, action: Action): Promise<Result<unknown>> {
  switch (action.type) {
    case 'list': return sessions.list(action.cursor);
    case 'read': return sessions.read(action.sessionId);
    case 'create': return sessions.create(action.cwd, action.agent);
    case 'interrupt': return sessions.interrupt(action.sessionId);
    case 'models': return sessions.models(action.cursor);
    case 'send': return sessions.send(action.sessionId, action.text, action.model);
    case 'approve': return sessions.approve(action.sessionId, action.approvalId, action.decision);
  }
}

type Connection = Readonly<{ socket: WebSocket; device: Device; pending: Map<string, (value: Result<unknown>) => void> }>;

export class DeviceConnections {
  private readonly connections = new Map<string, Connection>();
  private readonly websocket = new WebSocketServer({ noServer: true, maxPayload: 4 * 1024 * 1024 });
  private readonly state: StateStore;
  private readonly heartbeat: ReturnType<typeof setInterval>;

  constructor(server: Server, state: StateStore) {
    this.state = state;
    server.on('upgrade', (request, socket, head) => this.upgrade(request, socket, head));
    this.heartbeat = setInterval(() => {
      for (const connection of this.connections.values()) {
        if (Date.now() - connection.device.last_seen > 90000) connection.socket.terminate();
        else connection.socket.ping();
      }
    }, 30000);
    this.heartbeat.unref();
  }

  private upgrade(request: IncomingMessage, socket: Duplex, head: Buffer): void {
    const token = request.headers.authorization?.replace(/^Bearer /, '');
    const device = token ? this.state.authenticateDevice(token) : null;
    if (request.url !== '/agent' || request.headers.origin || !device) {
      socket.end('HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\n');
      return;
    }
    this.websocket.handleUpgrade(request, socket, head, websocket => {
      this.connections.get(device.id)?.socket.close(1000, 'Reconnected');
      const connection: Connection = { socket: websocket, device: { ...device, last_seen: Date.now() }, pending: new Map() };
      this.connections.set(device.id, connection);
      this.state.seen(device.id);
      websocket.on('error', () => {});
      websocket.on('pong', () => {
        if (this.connections.get(device.id)?.socket !== websocket) return;
        this.connections.set(device.id, { ...connection, device: { ...device, last_seen: Date.now() } });
        this.state.seen(device.id);
      });
      websocket.on('message', raw => {
        try {
          const reply = responseSchema.safeParse(JSON.parse(raw.toString()));
          if (!reply.success) { websocket.close(1008, 'Invalid response'); return; }
          connection.pending.get(reply.data.id)?.(reply.data.result);
          connection.pending.delete(reply.data.id);
        } catch { websocket.close(1008, 'Invalid response'); }
      });
      websocket.on('close', () => {
        if (this.connections.get(device.id)?.socket === websocket) this.connections.delete(device.id);
        for (const resolve of connection.pending.values()) resolve(failure(503, 'Your Mac disconnected. Refresh before sending again.'));
        connection.pending.clear();
      });
    });
  }

  online(id: string): boolean { return this.connections.get(id)?.socket.readyState === WebSocket.OPEN; }

  async request(device: Device, action: Action): Promise<Result<unknown>> {
    const connection = this.connections.get(device.id);
    if (!connection || !this.online(device.id)) return failure(503, 'This device is offline. Open Relay on your Mac to reconnect.');
    if (connection.pending.size >= 16) return failure(429, 'This device is busy. Try again shortly.');
    const id = randomUUID();
    return new Promise(resolve => {
      const timer = setTimeout(() => {
        connection.pending.delete(id);
        resolve(failure(504, 'Your Mac did not confirm the request. Refresh before sending again.'));
      }, 25000);
      connection.pending.set(id, value => { clearTimeout(timer); resolve(value); });
      connection.socket.send(JSON.stringify({ id, action }));
    });
  }

  disconnect(id: string): void { this.connections.get(id)?.socket.close(1008, 'Device removed'); }
  close(): void {
    clearInterval(this.heartbeat);
    for (const connection of this.connections.values()) connection.socket.terminate();
    this.websocket.close();
  }
}
