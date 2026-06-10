<script lang="ts">
  const API = "http://127.0.0.1:8765";

  type Status = "idle" | "pending" | "running" | "done" | "error" | "offline";

  let topic = $state("");
  let status = $state<Status>("idle");
  let contentHtml = $state("");
  let errorMsg = $state("");
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
</script>

<main>
  {#if status === "offline"}
    <div class="banner error">
      Prisma server is not running. Start it with: <code>prisma serve</code>
    </div>
  {/if}

  <div class="input-row">
    <input
      bind:value={topic}
      placeholder="Research topic…"
      disabled={status === "pending" || status === "running"}
    />
    <button
      onclick={startReview}
      disabled={!topic.trim() || status === "pending" || status === "running"}
    >
      {status === "pending" || status === "running" ? "Running…" : "Review"}
    </button>
  </div>

  {#if status === "error"}
    <div class="banner error">{errorMsg}</div>
  {/if}

  {#if status === "done" && contentHtml}
    <div class="report">
      {@html contentHtml}
    </div>
  {/if}
</main>

<style>
  main {
    display: flex;
    flex-direction: column;
    height: 100vh;
    font-family: Inter, sans-serif;
  }

  .input-row {
    display: flex;
    gap: 8px;
    padding: 12px 16px;
    border-bottom: 1px solid #e0e0e0;
  }

  input {
    flex: 1;
    padding: 8px 12px;
    border: 1px solid #ccc;
    border-radius: 6px;
    font-size: 14px;
  }

  button {
    padding: 8px 20px;
    border: none;
    border-radius: 6px;
    background: #3b82f6;
    color: white;
    font-size: 14px;
    cursor: pointer;
  }

  button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .banner {
    padding: 10px 16px;
    font-size: 13px;
  }

  .banner.error {
    background: #fef2f2;
    color: #b91c1c;
    border-bottom: 1px solid #fca5a5;
  }

  .report {
    flex: 1;
    overflow-y: auto;
    padding: 24px;
  }

  code {
    font-family: monospace;
    background: #f3f4f6;
    padding: 1px 4px;
    border-radius: 3px;
  }
</style>
