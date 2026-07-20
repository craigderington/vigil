import { createResource, createSignal, For, Show, type Component } from "solid-js";
import * as api from "../api";

/**
 * Reports screen (P4.4 §13/Task 8): month-card grid of previously generated
 * monthly incident reports, a "Generate a report" form (month picker, for
 * back-filling any past month — `generateReport` is idempotent per
 * `period_start` on the backend), and an in-app viewer that renders the
 * self-contained report HTML in an `<iframe srcdoc>` for style isolation
 * from the app's own navy theme (§13.3).
 */

const Reports: Component = () => {
  const [reports, { refetch }] = createResource(() => api.listReports().catch(() => [] as api.ReportCard[]));
  const [period, setPeriod] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [genNote, setGenNote] = createSignal<string | null>(null);
  const [openId, setOpenId] = createSignal<number | null>(null);
  const [html] = createResource(openId, (id) => (id != null ? api.reportHtml(id).catch(() => "") : ""));

  async function handleGenerate() {
    if (!period().trim()) return;
    setBusy(true);
    setGenNote(null);
    try {
      await api.generateReport(period().trim());
      setPeriod("");
      refetch();
    } catch (e: any) {
      setGenNote(e?.message ?? "Failed to generate report.");
    } finally {
      setBusy(false);
    }
  }

  async function handleDelete(id: number) {
    try {
      await api.deleteReport(id);
      if (openId() === id) setOpenId(null);
      refetch();
    } catch {
      // leave the card in the list so the operator can retry
    }
  }

  async function handleEmail(id: number) {
    try {
      await api.emailReport(id);
      refetch();
    } catch {
      // leave the card in the list so the operator can retry
    }
  }

  return (
    <div class="settings-view reports-view">
      <h2 class="settings-title">Reports</h2>

      <section class="form-section settings-section">
        <h3 class="form-section-title">Generate a report</h3>
        <div class="form-field">
          <label for="report-month">Month (YYYY-MM)</label>
          <input
            id="report-month"
            type="month"
            value={period()}
            onInput={(e) => setPeriod(e.currentTarget.value)}
          />
        </div>
        <div class="detail-actions">
          <button type="button" class="btn-accent" disabled={busy()} onClick={handleGenerate}>
            {busy() ? "Generating…" : "Generate report"}
          </button>
        </div>
        <Show when={genNote()}>
          <div class="test-result mono">{genNote()}</div>
        </Show>
      </section>

      <section class="form-section settings-section">
        <h3 class="form-section-title">Past reports</h3>
        <Show when={(reports() ?? []).length === 0}>
          <p class="settings-note">No reports yet.</p>
        </Show>
        <For each={reports() ?? []}>
          {(r) => (
            <div class="notif-row">
              <button type="button" class="btn-link" onClick={() => setOpenId(r.id)}>
                {r.label}
              </button>
              <span class="settings-note mono">
                {r.headline?.uptime_pct != null ? `${r.headline.uptime_pct}%` : "—"}
              </span>
              <span class="settings-note">{r.headline?.incidents ?? 0} incidents</span>
              <a class="btn-link" href={`/api/reports/${r.id}/html`} target="_blank" rel="noreferrer">
                Export HTML
              </a>
              <button type="button" class="btn-link" onClick={() => handleEmail(r.id)}>
                Email now
              </button>
              <button type="button" class="btn-link danger" onClick={() => handleDelete(r.id)}>
                Delete
              </button>
            </div>
          )}
        </For>
      </section>

      <Show when={openId() != null}>
        <section class="form-section settings-section">
          <h3 class="form-section-title">Report</h3>
          <iframe
            title="report"
            srcdoc={html() ?? ""}
            style="width:100%;height:70vh;border:1px solid var(--border-default);border-radius:10px;background:#fff"
          />
        </section>
      </Show>
    </div>
  );
};

export default Reports;
