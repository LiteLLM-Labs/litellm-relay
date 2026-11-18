import { homedir, hostname } from 'node:os';
import { chmod, mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline/promises';
import { z } from 'zod';
import { CodexSessions } from './codex.ts';
import { ClaudeSessions } from './claude.ts';
import { findClaudeExecutable, SdkClaudeRuntime } from './claude-runtime.ts';
import { RemoteSessions } from './sessions.ts';
import { connectDevice } from './connector.ts';

const credentialsSchema = z.object({ endpoint: z.string().url(), deviceId: z.string().uuid(), deviceToken: z.string().min(40), gateway: z.string().url() });
const endpoint = new URL(process.env.LITELLM_REMOTE_URL ?? 'http://127.0.0.1:4174');
if (endpoint.protocol !== 'https:' && !['localhost', '127.0.0.1', '[::1]'].includes(endpoint.hostname)) throw new Error('LITELLM_REMOTE_URL must use HTTPS except on localhost.');
if (endpoint.username || endpoint.password || endpoint.search || endpoint.hash || endpoint.pathname !== '/') throw new Error('Use the LiteLLM Remote service origin.');
const file = resolve(process.env.REMOTE_DEVICE_FILE ?? '.local/connector.json');

async function request(path: string, input: Readonly<Record<string, string>>, token?: string): Promise<unknown> {
  const response = await fetch(new URL(path, endpoint), { method: 'POST', redirect: 'error',
    headers: { 'Content-Type': 'application/json', 'X-LiteLLM-Client': 'relay', ...(token ? { Authorization: `Bearer ${token}` } : {}) },
    body: JSON.stringify(input), signal: AbortSignal.timeout(15000) });
  if (!response.ok) throw new Error(`Remote sign-in failed (HTTP ${response.status}).`);
  return response.json();
}

async function enroll() {
  const start = z.object({ flowToken: z.string(), code: z.string(), verificationUrl: z.string().url() }).parse(await request('/api/auth/start', {}));
  console.log(`Sign in to pair this Mac. Verification code ${start.code}`);
  console.log(start.verificationUrl);
  const browser = spawn(process.platform === 'darwin' ? 'open' : 'xdg-open', [start.verificationUrl], { stdio: 'ignore' });
  browser.on('error', () => {});
  const terminal = createInterface({ input: process.stdin, output: process.stdout });
  let team: string | undefined;
  try {
    for (let attempt = 0; attempt < 200; attempt += 1) {
      const polled = z.object({ status: z.string(), token: z.string().optional(),
        teams: z.array(z.object({ id: z.string(), name: z.string() })).optional() }).parse(
        await request('/api/auth/poll', { flowToken: start.flowToken, ...(team ? { team } : {}) }));
      if (polled.status === 'teams' && polled.teams?.length) {
        console.log(polled.teams.map((value, index) => `${index + 1}. ${value.name}`).join('\n'));
        const selected = Number(await terminal.question('Choose your team number '));
        team = polled.teams[selected - 1]?.id;
        continue;
      }
      if (polled.status === 'ready' && polled.token) {
        const device = credentialsSchema.omit({ endpoint: true }).parse(await request('/api/devices', { name: hostname() }, polled.token));
        await request('/api/auth/logout', {}, polled.token);
        const credentials = { ...device, endpoint: endpoint.origin };
        await mkdir(dirname(file), { recursive: true, mode: 0o700 });
        await writeFile(file, JSON.stringify(credentials), { mode: 0o600 });
        await chmod(file, 0o600);
        return credentials;
      }
      await new Promise(resolve => setTimeout(resolve, 3000));
    }
    throw new Error('Sign-in expired. Start the connector again.');
  } finally { terminal.close(); }
}

const saved = await readFile(file, 'utf8').catch(() => null);
const parsed = (() => {
  if (!saved) return null;
  try { return credentialsSchema.safeParse(JSON.parse(saved)); } catch { return null; }
})();
const handoffIndex = process.argv.indexOf('--handoff');
if (handoffIndex !== -1 && (!parsed?.success || parsed.data.endpoint !== endpoint.origin)) throw new Error('Use the enrolled Mac’s LITELLM_REMOTE_URL and REMOTE_DEVICE_FILE before handing over a session.');
const credentials = parsed?.success && parsed.data.endpoint === endpoint.origin && !process.argv.includes('--enroll') ? parsed.data : await enroll();
const codexHome = process.env.CODEX_HOME ?? resolve(homedir(), '.codex');
const codex = new CodexSessions(process.env.CODEX_CONTROL_SOCKET ?? `${codexHome}/app-server-control/app-server-control.sock`,
  process.env.LITELLM_MODEL_PROVIDER ?? 'litellm', credentials.gateway);
const claude = new ClaudeSessions(new SdkClaudeRuntime(await findClaudeExecutable(), credentials.gateway),
  join(dirname(file), `claude-${credentials.deviceId}`), process.env.REMOTE_CLAUDE_MODEL ?? 'opus');
if (handoffIndex !== -1) {
  const handed = await claude.handoff(process.argv[handoffIndex + 1] ?? '');
  if (handed.ok) console.log('This Claude session is ready in LiteLLM Remote. Keep the Mac connector running.');
  else { console.error(handed.error); process.exitCode = 1; }
} else {
  const sessions = new RemoteSessions({ codex, claude });
  const connector = connectDevice({ endpoint: credentials.endpoint, token: credentials.deviceToken, sessions, onStatus: console.log });
  function stop(): void { connector.stop(); codex.close(); claude.close(); }
  process.once('SIGTERM', stop);
  process.once('SIGINT', stop);
}
