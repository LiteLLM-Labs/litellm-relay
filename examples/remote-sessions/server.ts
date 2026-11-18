import { hostname, homedir, networkInterfaces } from 'node:os';
import { resolve } from 'node:path';
import { createApp } from './app.ts';
import { Authentication } from './auth.ts';
import { CodexSessions } from './codex.ts';
import { LiteLLMGateway } from './gateway.ts';

const gatewayUrl = process.env.LITELLM_GATEWAY_URL?.replace(/\/v1\/?$/, '').replace(/\/$/, '');
if (!gatewayUrl || new URL(gatewayUrl).protocol !== 'https:') {
  console.error('Set LITELLM_GATEWAY_URL to your HTTPS LiteLLM gateway URL.');
  process.exit(1);
}
const port = Number(process.env.REMOTE_PORT ?? '4174');
const host = process.env.REMOTE_HOST ?? '127.0.0.1';
const addresses = Object.values(networkInterfaces()).flatMap(value => value ?? [])
  .filter(value => value.family === 'IPv4' && !value.internal).map(value => value.address);
const socketPath = process.env.CODEX_CONTROL_SOCKET ?? `${homedir()}/.codex/app-server-control/app-server-control.sock`;
const sessions = new CodexSessions(socketPath, process.env.LITELLM_MODEL_PROVIDER ?? 'litellm');
const auth = new Authentication(new LiteLLMGateway(gatewayUrl), process.env.LITELLM_API_KEY ?? '', resolve('.local/owner.json'));
await auth.load();
const app = createApp({
  auth, sessions, gatewayUrl, machineName: hostname(),
  allowedHosts: new Set(['localhost', '127.0.0.1', '[::1]', hostname(), `${hostname()}.local`, ...addresses]),
  phoneUrls: host === '127.0.0.1' ? [] : addresses.map(address => `http://${address}:${port}`),
});
app.listen(port, host, () => {
  console.log(`LiteLLM Remote bridge is running at http://localhost:${port}`);
  console.log('Existing Codex sessions are read-only. No session will be resumed or restarted.');
});
app.on('error', error => { console.error(error.message); process.exitCode = 1; });
process.on('SIGTERM', () => { sessions.close(); app.close(); });
process.on('SIGINT', () => { sessions.close(); app.close(); });
