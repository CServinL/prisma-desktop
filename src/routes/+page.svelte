<script lang="ts">
  const API = "http://127.0.0.1:8765";

  type Status = "idle" | "pending" | "running" | "done" | "error" | "offline";

  let topic = $state("");
  let status = $state<Status>("idle");
  let contentHtml = $state("");
  let errorMsg = $state("");
  let currentTopic = $state("");
  let pollTimer: ReturnType<typeof setInterval> | null = null;

  async function checkServer(): Promise<boolean> {
    try {
      const r = await fetch(`${API}/health`, { signal: AbortSignal.timeout(2000) });
      return r.ok;
    } catch {
      return false;
    }
  }

  async function startReview() {
    const up = await checkServer();
    if (!up) { status = "offline"; return; }

    status = "pending";
    contentHtml = "";
    errorMsg = "";
    currentTopic = topic;

    const res = await fetch(`${API}/review`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ topic }),
    });
    if (!res.ok) { status = "error"; errorMsg = await res.text(); return; }

    const { job_id } = await res.json();
    pollTimer = setInterval(() => poll(job_id), 2000);
  }

  async function poll(jobId: string) {
    try {
      const res = await fetch(`${API}/review/${jobId}`);
      const job = await res.json();
      if (job.status === "done") {
        clearInterval(pollTimer!);
        status = "done";
        contentHtml = job.content_html;
      } else if (job.status === "error") {
        clearInterval(pollTimer!);
        status = "error";
        errorMsg = job.errors?.join(", ") ?? "unknown error";
      } else {
        status = job.status;
      }
    } catch {
      clearInterval(pollTimer!);
      status = "error";
      errorMsg = "lost connection to server";
    }
  }

  const busy = $derived(status === "pending" || status === "running");
</script>

<div class="shell">
  <!-- toolbar -->
  <div class="toolbar">
    <span class="logo">Prisma</span>
    <div class="search-row">
      <input
        bind:value={topic}
        placeholder="Research topic…"
        disabled={busy}
        onkeydown={(e) => e.key === "Enter" && !busy && topic.trim() && startReview()}
      />
      <button onclick={startReview} disabled={!topic.trim() || busy}>
        {busy ? "Running…" : "Review"}
      </button>
    </div>
  </div>

  <!-- status bar -->
  {#if status === "offline"}
    <div class="notice error">
      Prisma server not running — start it with <code>prisma serve</code>
    </div>
  {:else if status === "error"}
    <div class="notice error">{errorMsg}</div>
  {:else if busy}
    <div class="notice info">
      <span class="spinner"></span>
      Reviewing <em>{currentTopic}</em>…
    </div>
  {/if}

  <!-- content -->
  <div class="content">
    {#if status === "idle"}
      <div class="empty">
        <p>Enter a research topic above to generate a literature review.</p>
      </div>
    {:else if status === "done" && contentHtml}
      <iframe srcdoc={contentHtml} title="Literature review: {currentTopic}" sandbox=""></iframe>
    {/if}
  </div>
</div>

<style>
  :global(*, *::before, *::after) { box-sizing: border-box; margin: 0; padding: 0; }
  :global(html, body) {
    height: 100%;
    background: #0a0e1a;
    color: #e8edf8;
    font-family: Inter, "Segoe UI", system-ui, sans-serif;
  }

  .shell {
    display: flex;
    flex-direction: column;
    height: 100vh;
    background: #0a0e1a;
  }

  /* ── Toolbar ────────────────────────────────────────────────────────────── */
  .toolbar {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 10px 16px;
    background: #080c16;
    border-bottom: 1px solid #1a2d4a;
    flex-shrink: 0;
  }

  .logo {
    font-size: 15px;
    font-weight: 700;
    color: #4a9eff;
    letter-spacing: 0.05em;
    white-space: nowrap;
  }

  .search-row {
    display: flex;
    gap: 8px;
    flex: 1;
  }

  input {
    flex: 1;
    padding: 7px 12px;
    background: #0d1320;
    border: 1px solid #1a2d4a;
    border-radius: 6px;
    color: #e8edf8;
    font-size: 13px;
    font-family: inherit;
    outline: none;
    transition: border-color 0.15s;
  }

  input:focus { border-color: #4a9eff; }
  input::placeholder { color: #3d5470; }
  input:disabled { opacity: 0.5; }

  button {
    padding: 7px 18px;
    background: #1a3a6a;
    border: 1px solid #2a5aaa;
    border-radius: 6px;
    color: #a8c8ff;
    font-size: 13px;
    font-family: inherit;
    font-weight: 500;
    cursor: pointer;
    white-space: nowrap;
    transition: background 0.15s, border-color 0.15s;
  }

  button:hover:not(:disabled) {
    background: #234a8a;
    border-color: #4a9eff;
    color: #ffffff;
  }

  button:disabled { opacity: 0.4; cursor: not-allowed; }

  /* ── Notice bar ─────────────────────────────────────────────────────────── */
  .notice {
    padding: 8px 16px;
    font-size: 12px;
    display: flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
  }

  .notice.error {
    background: #1a0a0a;
    color: #f87171;
    border-bottom: 1px solid #3a1010;
  }

  .notice.info {
    background: #080f1e;
    color: #7ab4f5;
    border-bottom: 1px solid #1a2d4a;
  }

  .notice em { color: #a8c8ff; font-style: normal; }

  code {
    font-family: "JetBrains Mono", "Fira Code", "Courier New", monospace;
    font-size: 11px;
    background: #0d1829;
    color: #82bfff;
    padding: 1px 5px;
    border-radius: 3px;
  }

  /* ── Spinner ────────────────────────────────────────────────────────────── */
  .spinner {
    display: inline-block;
    width: 12px;
    height: 12px;
    border: 2px solid #1a3a6a;
    border-top-color: #4a9eff;
    border-radius: 50%;
    animation: spin 0.7s linear infinite;
    flex-shrink: 0;
  }

  @keyframes spin { to { transform: rotate(360deg); } }

  /* ── Content area ───────────────────────────────────────────────────────── */
  .content {
    flex: 1;
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }

  iframe {
    flex: 1;
    width: 100%;
    border: none;
    background: #0a0e1a;
  }

  .empty {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    color: #2a4060;
    font-size: 14px;
  }
</style>
