import { mkdir, chmod } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { createApp } from './app.ts';
import { Authentication } from './auth.ts';
import { LiteLLMGateway } from './gateway.ts';
import { StateStore } from './state.ts';

if (!process.env.LITELLM_GATEWAY_URL) throw new Error('Set LITELLM_GATEWAY_URL to your gateway URL.');
const gateway = new URL(process.env.LITELLM_GATEWAY_URL);
const publicUrl = new URL(process.env.REMOTE_PUBLIC_URL ?? 'http://127.0.0.1:4174');
if (gateway.protocol !== 'https:' || gateway.username || gateway.password || gateway.search || gateway.hash) throw new Error('Set LITELLM_GATEWAY_URL to your HTTPS gateway URL.');
if (publicUrl.protocol !== 'https:' && !['localhost', '127.0.0.1', '[::1]'].includes(publicUrl.hostname)) throw new Error('REMOTE_PUBLIC_URL must use HTTPS except on localhost.');
const gatewayUrl = gateway.toString().replace(/\/+$/, '').replace(/\/v1$/, '');
const file = resolve(process.env.REMOTE_STATE_PATH ?? '.local/control.sqlite');
await mkdir(dirname(file), { recursive: true, mode: 0o700 });
const state = new StateStore(file);
await chmod(file, 0o600);
const { server, devices } = createApp({ state, auth: new Authentication(new LiteLLMGateway(gatewayUrl), state, gatewayUrl), gatewayUrl, publicUrl: publicUrl.origin });
server.listen(Number(process.env.REMOTE_PORT ?? '4174'), process.env.REMOTE_HOST ?? '127.0.0.1', () => {
  console.log(`LiteLLM Remote service is running at ${publicUrl.origin}`);
  console.log('Mac connectors connect outbound. No Codex socket or Mac LAN port is exposed.');
});
function stop(): void { devices.close(); server.close(() => { state.close(); }); }
process.once('SIGTERM', stop);
process.once('SIGINT', stop);
