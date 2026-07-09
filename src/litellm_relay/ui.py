from __future__ import annotations


DASHBOARD_HTML = r"""<!doctype html>
<html lang="en" class="dark">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>LiteLLM Relay</title>
  <style>
    :root {
      --background: 240 10% 3.9%;
      --foreground: 0 0% 98%;
      --card: 240 10% 3.9%;
      --card-foreground: 0 0% 98%;
      --popover: 240 10% 3.9%;
      --popover-foreground: 0 0% 98%;
      --primary: 162 74% 45%;
      --primary-foreground: 164 95% 8%;
      --secondary: 240 3.7% 15.9%;
      --secondary-foreground: 0 0% 98%;
      --muted: 240 3.7% 15.9%;
      --muted-foreground: 240 5% 64.9%;
      --accent: 240 3.7% 15.9%;
      --accent-foreground: 0 0% 98%;
      --destructive: 0 62.8% 50.6%;
      --destructive-foreground: 0 0% 98%;
      --border: 240 3.7% 15.9%;
      --input: 240 3.7% 15.9%;
      --ring: 162 74% 45%;
      --radius: 0.625rem;
      --font-sans: "Geist", "Geist Fallback", ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      --font-mono: "Geist Mono", "Geist Mono Fallback", ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    }

    * { box-sizing: border-box; }

    body {
      margin: 0;
      min-height: 100vh;
      color: hsl(var(--foreground));
      background: hsl(var(--background));
      font-family: var(--font-sans);
      letter-spacing: 0;
    }

    button, input, select {
      font: inherit;
    }

    .app {
      min-height: 100vh;
      display: grid;
      grid-template-columns: 280px minmax(0, 1fr);
    }

    .sidebar {
      border-right: 1px solid hsl(var(--border));
      background: hsl(240 10% 4.5%);
      padding: 20px;
      display: flex;
      flex-direction: column;
      gap: 20px;
    }

    .brand {
      display: flex;
      align-items: center;
      gap: 12px;
      min-height: 40px;
    }

    .logo {
      width: 32px;
      height: 32px;
      border: 1px solid hsl(var(--primary));
      border-radius: 8px;
      display: grid;
      place-items: center;
      color: hsl(var(--primary));
      font-family: var(--font-mono);
      box-shadow: 0 0 0 1px hsl(var(--primary) / 0.16), 0 0 28px hsl(var(--primary) / 0.16);
    }

    .brand-title {
      font-weight: 650;
      line-height: 1.1;
    }

    .brand-subtitle {
      color: hsl(var(--muted-foreground));
      font-size: 12px;
      margin-top: 3px;
    }

    .nav {
      display: grid;
      gap: 6px;
    }

    .nav-item {
      border: 1px solid transparent;
      border-radius: calc(var(--radius) - 2px);
      padding: 10px 12px;
      color: hsl(var(--muted-foreground));
      background: transparent;
      text-align: left;
      font-size: 14px;
    }

    .nav-item.active {
      color: hsl(var(--foreground));
      background: hsl(var(--accent));
      border-color: hsl(var(--border));
    }

    .sidebar-card {
      margin-top: auto;
      border: 1px solid hsl(var(--border));
      border-radius: var(--radius);
      background: hsl(var(--card));
      padding: 14px;
      color: hsl(var(--muted-foreground));
      font-size: 13px;
      line-height: 1.5;
    }

    .content {
      min-width: 0;
      padding: 24px;
      display: flex;
      flex-direction: column;
      gap: 20px;
    }

    .header {
      display: flex;
      align-items: flex-start;
      justify-content: space-between;
      gap: 18px;
    }

    h1 {
      margin: 0;
      font-size: 26px;
      line-height: 1.2;
      font-weight: 720;
    }

    .lead {
      margin: 6px 0 0;
      color: hsl(var(--muted-foreground));
      font-size: 14px;
    }

    .actions {
      display: flex;
      align-items: center;
      gap: 10px;
      flex-wrap: wrap;
      justify-content: flex-end;
    }

    .button {
      min-height: 36px;
      border-radius: calc(var(--radius) - 2px);
      border: 1px solid hsl(var(--border));
      padding: 0 12px;
      background: hsl(var(--secondary));
      color: hsl(var(--secondary-foreground));
      display: inline-flex;
      align-items: center;
      gap: 8px;
      cursor: pointer;
    }

    .button.primary {
      background: hsl(var(--primary));
      color: hsl(var(--primary-foreground));
      border-color: hsl(var(--primary));
      font-weight: 650;
    }

    .button.ghost {
      background: transparent;
    }

    .button:disabled {
      opacity: 0.55;
      cursor: not-allowed;
    }

    .grid {
      display: grid;
      grid-template-columns: repeat(4, minmax(0, 1fr));
      gap: 14px;
    }

    .card {
      border: 1px solid hsl(var(--border));
      border-radius: var(--radius);
      background: hsl(var(--card));
      color: hsl(var(--card-foreground));
      box-shadow: 0 1px 2px hsl(0 0% 0% / 0.18);
    }

    .metric {
      padding: 16px;
    }

    .metric-label {
      color: hsl(var(--muted-foreground));
      font-size: 12px;
      text-transform: uppercase;
      font-weight: 650;
      letter-spacing: 0.06em;
    }

    .metric-value {
      margin-top: 8px;
      font: 700 28px/1 var(--font-mono);
    }

    .metric-foot {
      margin-top: 8px;
      color: hsl(var(--muted-foreground));
      font-size: 12px;
      white-space: nowrap;
      overflow: hidden;
      text-overflow: ellipsis;
    }

    .toolbar {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 12px;
      padding: 14px;
    }

    .filters {
      display: flex;
      align-items: center;
      gap: 10px;
      flex-wrap: wrap;
    }

    .input, .select {
      min-height: 36px;
      border-radius: calc(var(--radius) - 2px);
      border: 1px solid hsl(var(--input));
      background: hsl(var(--background));
      color: hsl(var(--foreground));
      padding: 0 11px;
      outline: none;
    }

    .input:focus, .select:focus {
      border-color: hsl(var(--ring));
      box-shadow: 0 0 0 3px hsl(var(--ring) / 0.18);
    }

    .input {
      width: min(340px, 52vw);
    }

    .table-wrap {
      overflow: auto;
      border-top: 1px solid hsl(var(--border));
    }

    table {
      width: 100%;
      border-collapse: collapse;
      min-width: 920px;
    }

    th, td {
      border-bottom: 1px solid hsl(var(--border));
      padding: 12px 14px;
      text-align: left;
      vertical-align: middle;
      font-size: 13px;
    }

    th {
      color: hsl(var(--muted-foreground));
      font-weight: 650;
      background: hsl(240 4% 8%);
      position: sticky;
      top: 0;
      z-index: 1;
    }

    td.mono, .mono {
      font-family: var(--font-mono);
    }

    .badge {
      display: inline-flex;
      align-items: center;
      gap: 6px;
      border: 1px solid hsl(var(--border));
      border-radius: 999px;
      padding: 3px 8px;
      font-size: 12px;
      font-weight: 650;
      white-space: nowrap;
      background: hsl(var(--secondary));
      color: hsl(var(--secondary-foreground));
    }

    .badge.green {
      border-color: hsl(var(--primary) / 0.35);
      background: hsl(var(--primary) / 0.12);
      color: hsl(162 80% 70%);
    }

    .badge.red {
      border-color: hsl(var(--destructive) / 0.42);
      background: hsl(var(--destructive) / 0.14);
      color: hsl(0 84% 72%);
    }

    .badge.yellow {
      border-color: hsl(38 92% 50% / 0.35);
      background: hsl(38 92% 50% / 0.12);
      color: hsl(42 92% 72%);
    }

    .dot {
      width: 7px;
      height: 7px;
      border-radius: 999px;
      background: currentColor;
    }

    .empty {
      min-height: 220px;
      display: grid;
      place-items: center;
      color: hsl(var(--muted-foreground));
      text-align: center;
      padding: 28px;
    }

    .empty strong {
      color: hsl(var(--foreground));
      display: block;
      margin-bottom: 6px;
    }

    .details {
      padding: 16px;
      display: grid;
      gap: 12px;
    }

    .code {
      background: hsl(240 10% 6%);
      border: 1px solid hsl(var(--border));
      border-radius: calc(var(--radius) - 2px);
      padding: 12px;
      overflow: auto;
      color: hsl(var(--muted-foreground));
      font: 12px/1.6 var(--font-mono);
    }

    .toast {
      position: fixed;
      right: 20px;
      bottom: 20px;
      max-width: 360px;
      border: 1px solid hsl(var(--border));
      border-radius: var(--radius);
      background: hsl(var(--popover));
      color: hsl(var(--popover-foreground));
      padding: 12px 14px;
      box-shadow: 0 16px 40px hsl(0 0% 0% / 0.35);
      display: none;
      font-size: 13px;
    }

    .toast.show {
      display: block;
    }

    @media (max-width: 980px) {
      .app { grid-template-columns: 1fr; }
      .sidebar {
        border-right: 0;
        border-bottom: 1px solid hsl(var(--border));
      }
      .sidebar-card { margin-top: 0; }
      .grid { grid-template-columns: repeat(2, minmax(0, 1fr)); }
      .header { flex-direction: column; }
      .actions { justify-content: flex-start; }
    }

    @media (max-width: 620px) {
      .content { padding: 16px; }
      .grid { grid-template-columns: 1fr; }
      .toolbar { align-items: stretch; flex-direction: column; }
      .input { width: 100%; }
    }
  </style>
</head>
<body>
  <div class="app">
    <aside class="sidebar">
      <div class="brand">
        <div class="logo">L</div>
        <div>
          <div class="brand-title">LiteLLM Relay</div>
          <div class="brand-subtitle">Local traffic proxy</div>
        </div>
      </div>
      <div class="nav">
        <button class="nav-item active">Intercepted traffic</button>
        <button class="nav-item" onclick="scrollToCard('config-card')">Proxy setup</button>
        <button class="nav-item" onclick="scrollToCard('raw-card')">Raw event</button>
      </div>
      <div class="sidebar-card">
        <strong style="color:hsl(var(--foreground));">Default mode</strong><br>
        HTTPS payloads stay encrypted. Relay records CONNECT metadata and optional
        redacted shadow calls through LiteLLM Gateway.
      </div>
    </aside>

    <main class="content">
      <header class="header">
        <div>
          <h1>Intercepted AI traffic</h1>
          <p class="lead">Live local view of AI traffic routed through this Relay process.</p>
        </div>
        <div class="actions">
          <span id="live-badge" class="badge green"><span class="dot"></span> Live</span>
          <button id="pause-button" class="button ghost" type="button">Pause</button>
          <button id="copy-pac" class="button" type="button">Copy PAC URL</button>
          <button id="refresh-button" class="button primary" type="button">Refresh</button>
        </div>
      </header>

      <section class="grid" aria-label="Traffic metrics">
        <div class="card metric">
          <div class="metric-label">Events</div>
          <div id="metric-events" class="metric-value">0</div>
          <div class="metric-foot">Total log rows loaded</div>
        </div>
        <div class="card metric">
          <div class="metric-label">AI matches</div>
          <div id="metric-ai" class="metric-value">0</div>
          <div class="metric-foot">OpenAI, Anthropic, Notion, and configured hosts</div>
        </div>
        <div class="card metric">
          <div class="metric-label">Shadowed</div>
          <div id="metric-shadow" class="metric-value">0</div>
          <div class="metric-foot">Gateway shadow attempts</div>
        </div>
        <div class="card metric">
          <div class="metric-label">Bytes</div>
          <div id="metric-bytes" class="metric-value">0</div>
          <div class="metric-foot">Closed tunnel bytes</div>
        </div>
      </section>

      <section class="card">
        <div class="toolbar">
          <div class="filters">
            <input id="search" class="input" placeholder="Filter host, event id, method..." />
            <select id="kind-filter" class="select">
              <option value="all">All events</option>
              <option value="ai">AI only</option>
              <option value="shadow">Shadow attempts</option>
              <option value="closed">Closed tunnels</option>
            </select>
          </div>
          <div class="mono" style="color:hsl(var(--muted-foreground)); font-size:12px;">
            Last update <span id="last-update">never</span>
          </div>
        </div>
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Time</th>
                <th>Event</th>
                <th>Host</th>
                <th>App</th>
                <th>Port</th>
                <th>Shadow</th>
                <th>Duration</th>
                <th>Bytes</th>
                <th>Event ID</th>
              </tr>
            </thead>
            <tbody id="events-body"></tbody>
          </table>
          <div id="empty" class="empty">
            <div>
              <strong>No intercepted traffic yet</strong>
              Route an app through the PAC URL or run:
              <div class="code" style="margin-top:12px; text-align:left;">curl -I -x http://127.0.0.1:4142 https://www.notion.so</div>
            </div>
          </div>
        </div>
      </section>

      <section id="config-card" class="card details">
        <div style="display:flex;align-items:center;justify-content:space-between;gap:12px;">
          <div>
            <div style="font-weight:650;">Proxy setup</div>
            <div style="color:hsl(var(--muted-foreground));font-size:13px;">Use this PAC URL to route supported AI apps through Relay.</div>
          </div>
          <span id="status-badge" class="badge">Loading</span>
        </div>
        <div id="config-code" class="code">Loading...</div>
      </section>

      <section id="raw-card" class="card details">
        <div>
          <div style="font-weight:650;">Selected raw event</div>
          <div style="color:hsl(var(--muted-foreground));font-size:13px;">Click a table row to inspect the redacted JSON event.</div>
        </div>
        <pre id="raw-event" class="code">No event selected.</pre>
      </section>
    </main>
  </div>

  <div id="toast" class="toast"></div>

  <script>
    const state = {
      events: [],
      paused: false,
      selected: null,
      status: null,
    };

    const $ = (id) => document.getElementById(id);

    function fmtTime(value) {
      if (!value) return "";
      const date = new Date(value);
      if (Number.isNaN(date.getTime())) return value;
      return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
    }

    function fmtBytes(value) {
      const n = Number(value || 0);
      if (n < 1024) return String(n);
      if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
      return `${(n / 1024 / 1024).toFixed(1)} MB`;
    }

    function toast(message) {
      const node = $("toast");
      node.textContent = message;
      node.classList.add("show");
      setTimeout(() => node.classList.remove("show"), 2200);
    }

    function badgeForEvent(event) {
      if (event.event === "connect_failed") return '<span class="badge red">failed</span>';
      if (event.event === "connect_closed") return '<span class="badge">closed</span>';
      if (event.ai_match) return '<span class="badge green">ai</span>';
      if (event.event === "relay_started") return '<span class="badge yellow">relay</span>';
      return '<span class="badge">event</span>';
    }

    function shadowBadge(event) {
      if (!event.shadow) return '<span class="badge">none</span>';
      if (event.shadow.ok) return '<span class="badge green">ok</span>';
      if (event.shadow.attempted) return '<span class="badge red">failed</span>';
      return '<span class="badge yellow">skipped</span>';
    }

    function filteredEvents() {
      const query = $("search").value.trim().toLowerCase();
      const kind = $("kind-filter").value;
      return state.events.filter((event) => {
        if (kind === "ai" && !event.ai_match) return false;
        if (kind === "shadow" && !event.shadow?.attempted) return false;
        if (kind === "closed" && event.event !== "connect_closed") return false;
        if (!query) return true;
        return JSON.stringify(event).toLowerCase().includes(query);
      });
    }

    function renderMetrics(events) {
      const ai = events.filter((event) => event.ai_match).length;
      const shadow = events.filter((event) => event.shadow?.attempted).length;
      const bytes = events.reduce((total, event) => total + Number(event.bytes_in || 0) + Number(event.bytes_out || 0), 0);
      $("metric-events").textContent = String(events.length);
      $("metric-ai").textContent = String(ai);
      $("metric-shadow").textContent = String(shadow);
      $("metric-bytes").textContent = fmtBytes(bytes);
    }

    function renderTable() {
      const events = filteredEvents();
      renderMetrics(state.events);
      const body = $("events-body");
      body.innerHTML = "";
      $("empty").style.display = events.length ? "none" : "grid";
      for (const event of events.slice().reverse()) {
        const row = document.createElement("tr");
        row.tabIndex = 0;
        row.style.cursor = "pointer";
        row.innerHTML = `
          <td class="mono">${fmtTime(event.timestamp)}</td>
          <td>${badgeForEvent(event)}</td>
          <td class="mono">${event.host || event.listen || "-"}</td>
          <td>${event.app ? `<span class="badge">${event.app}</span>` : "-"}</td>
          <td class="mono">${event.port || "-"}</td>
          <td>${shadowBadge(event)}</td>
          <td class="mono">${event.duration_ms ? `${event.duration_ms} ms` : "-"}</td>
          <td class="mono">${event.bytes_in || event.bytes_out ? `${fmtBytes(event.bytes_in)} / ${fmtBytes(event.bytes_out)}` : "-"}</td>
          <td class="mono">${event.event_id || "-"}</td>
        `;
        row.addEventListener("click", () => selectEvent(event));
        row.addEventListener("keydown", (evt) => {
          if (evt.key === "Enter") selectEvent(event);
        });
        body.appendChild(row);
      }
    }

    function selectEvent(event) {
      state.selected = event;
      $("raw-event").textContent = JSON.stringify(event, null, 2);
      scrollToCard("raw-card");
    }

    function renderStatus() {
      const status = state.status;
      if (!status) return;
      $("status-badge").className = status.shadow_enabled ? "badge green" : "badge yellow";
      $("status-badge").textContent = status.shadow_enabled ? "Shadow enabled" : "Shadow off";
      $("config-code").textContent = [
        `Dashboard: ${location.origin}/`,
        `PAC URL:   ${location.origin}/proxy.pac`,
        `Proxy:     ${status.listen}`,
        `Log file:  ${status.log_path}`,
        `AI domains: ${status.ai_domains.join(", ")}`,
      ].join("\n");
    }

    async function loadStatus() {
      const response = await fetch("/api/status", { cache: "no-store" });
      state.status = await response.json();
      renderStatus();
    }

    async function loadEvents() {
      if (state.paused) return;
      const response = await fetch("/api/events?limit=500", { cache: "no-store" });
      const payload = await response.json();
      state.events = payload.events || [];
      $("last-update").textContent = new Date().toLocaleTimeString();
      renderTable();
    }

    function scrollToCard(id) {
      document.getElementById(id)?.scrollIntoView({ behavior: "smooth", block: "nearest" });
    }

    $("refresh-button").addEventListener("click", () => {
      loadStatus();
      loadEvents();
    });
    $("pause-button").addEventListener("click", () => {
      state.paused = !state.paused;
      $("pause-button").textContent = state.paused ? "Resume" : "Pause";
      $("live-badge").className = state.paused ? "badge yellow" : "badge green";
      $("live-badge").innerHTML = `<span class="dot"></span> ${state.paused ? "Paused" : "Live"}`;
    });
    $("copy-pac").addEventListener("click", async () => {
      await navigator.clipboard.writeText(`${location.origin}/proxy.pac`);
      toast("PAC URL copied");
    });
    $("search").addEventListener("input", renderTable);
    $("kind-filter").addEventListener("change", renderTable);

    loadStatus().catch((error) => toast(`Status failed: ${error.message}`));
    loadEvents().catch((error) => toast(`Events failed: ${error.message}`));
    setInterval(loadEvents, 1500);
  </script>
</body>
</html>
"""
