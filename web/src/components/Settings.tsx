import { createSignal, onMount, Show, type Component } from "solid-js";
import * as api from "../api";

/**
 * Settings screen (Task 18): SMTP is edited as the single `email`
 * notification channel's `config` JSON — there is deliberately no password
 * field anywhere on this screen. The SMTP password is supplied to the
 * backend via a Docker secret at container startup and is never stored in
 * (or readable from) the app DB, so the UI can only ever configure
 * host/port/security/from/recipients and must say so plainly.
 *
 * Robust to `{}` / non-array responses: `getChannels()` and `getSettings()`
 * are both defended so a bare `{}` (e.g. an unconfigured backend, or a
 * test stub) falls back to sane defaults instead of throwing.
 */

const DEFAULT_RETENTION_DAYS = 30;

function asArray(v: unknown): any[] {
  return Array.isArray(v) ? v : [];
}

function parseAnchorsInput(text: string): string[] {
  return text
    .split(/[\n,]/)
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

function parseEmailConfig(raw: any): {
  host: string;
  port: number;
  security: string;
  from: string;
  to: string[];
} {
  let cfg: any = raw;
  if (typeof raw === "string") {
    try {
      cfg = JSON.parse(raw);
    } catch {
      cfg = {};
    }
  }
  cfg = cfg ?? {};
  return {
    host: typeof cfg.host === "string" ? cfg.host : "",
    port: typeof cfg.port === "number" ? cfg.port : 587,
    security: typeof cfg.security === "string" ? cfg.security : "starttls",
    from: typeof cfg.from === "string" ? cfg.from : "",
    to: asArray(cfg.to).filter((s) => typeof s === "string"),
  };
}

const Settings: Component = () => {
  const [emailChannel, setEmailChannel] = createSignal<any | null>(null);

  const [host, setHost] = createSignal("");
  const [port, setPort] = createSignal<number>(587);
  const [security, setSecurity] = createSignal("starttls");
  const [from, setFrom] = createSignal("");
  const [recipients, setRecipients] = createSignal("");

  const [anchorsText, setAnchorsText] = createSignal("");
  const [retentionDays, setRetentionDays] = createSignal<number>(DEFAULT_RETENTION_DAYS);

  const [saving, setSaving] = createSignal(false);
  const [saveNote, setSaveNote] = createSignal<string | null>(null);
  const [testing, setTesting] = createSignal(false);
  const [testResult, setTestResult] = createSignal<{ ok: boolean; error?: string | null } | null>(
    null,
  );
  const [anchorsSaving, setAnchorsSaving] = createSignal(false);
  const [anchorsSaved, setAnchorsSaved] = createSignal(false);
  const [retentionSaving, setRetentionSaving] = createSignal(false);
  const [retentionSaved, setRetentionSaved] = createSignal(false);

  function loadChannelIntoForm(ch: any) {
    const cfg = parseEmailConfig(ch?.config);
    setHost(cfg.host);
    setPort(cfg.port);
    setSecurity(cfg.security);
    setFrom(cfg.from);
    setRecipients(cfg.to.join(", "));
  }

  onMount(async () => {
    try {
      const list = await api.getChannels();
      const arr = asArray(list);
      const email = arr.find((c: any) => c?.type === "email") ?? null;
      if (email) {
        setEmailChannel(email);
        loadChannelIntoForm(email);
      }
    } catch {
      // No channels yet (or unreachable backend) — stay on defaults; the
      // form still renders so a first-time SMTP setup can proceed.
    }

    try {
      const s: any = await api.getSettings();
      setAnchorsText(asArray(s?.anchors).join(", "));
      setRetentionDays(typeof s?.retention_days === "number" ? s.retention_days : DEFAULT_RETENTION_DAYS);
    } catch {
      // stay on defaults
    }
  });

  function buildEmailConfig() {
    return {
      host: host(),
      port: Number(port()) || 0,
      security: security(),
      from: from(),
      to: recipients()
        .split(",")
        .map((s) => s.trim())
        .filter((s) => s.length > 0),
    };
  }

  async function handleSaveSmtp() {
    setSaving(true);
    setSaveNote(null);
    try {
      const config = buildEmailConfig();
      const existing = emailChannel();
      const saved =
        existing?.id != null
          ? await api.updateChannel(existing.id, { config })
          : await api.createChannel({ name: "Email", type: "email", config });
      if (saved?.id != null) {
        setEmailChannel(saved);
      }
      setSaveNote("Saved.");
    } catch (e: any) {
      setSaveNote(e?.message ?? "Failed to save SMTP settings.");
    } finally {
      setSaving(false);
    }
  }

  async function handleSendTest() {
    const ch = emailChannel();
    if (ch?.id == null) return;
    setTesting(true);
    setTestResult(null);
    try {
      const result = await api.testChannel(ch.id);
      setTestResult(result);
    } catch (e: any) {
      setTestResult({ ok: false, error: e?.message ?? "Failed to send test email." });
    } finally {
      setTesting(false);
    }
  }

  async function handleSaveAnchors() {
    setAnchorsSaving(true);
    setAnchorsSaved(false);
    try {
      await api.updateSettings({ anchors: parseAnchorsInput(anchorsText()) });
      setAnchorsSaved(true);
    } catch {
      // leave the field as typed so the operator can retry
    } finally {
      setAnchorsSaving(false);
    }
  }

  async function handleSaveRetention() {
    setRetentionSaving(true);
    setRetentionSaved(false);
    try {
      const days = Math.max(1, Number(retentionDays()) || DEFAULT_RETENTION_DAYS);
      await api.updateSettings({ retention_days: days });
      setRetentionSaved(true);
    } catch {
      // leave the field as typed so the operator can retry
    } finally {
      setRetentionSaving(false);
    }
  }

  return (
    <div class="settings-view">
      <h2 class="settings-title">Settings</h2>

      <section class="form-section settings-section">
        <h3 class="form-section-title">Email (SMTP)</h3>
        <p class="settings-note">Password managed via Docker secret.</p>

        <div class="form-field">
          <label for="set-smtp-host">Host</label>
          <input
            id="set-smtp-host"
            type="text"
            placeholder="smtp.example.com"
            value={host()}
            onInput={(e) => setHost(e.currentTarget.value)}
          />
        </div>
        <div class="form-field">
          <label for="set-smtp-port">Port</label>
          <input
            id="set-smtp-port"
            type="number"
            min={1}
            value={port()}
            onInput={(e) => setPort(Number(e.currentTarget.value) || 0)}
          />
        </div>
        <div class="form-field">
          <label for="set-smtp-security">Security</label>
          <select
            id="set-smtp-security"
            value={security()}
            onChange={(e) => setSecurity(e.currentTarget.value)}
          >
            <option value="none">None</option>
            <option value="starttls">STARTTLS</option>
            <option value="tls">TLS</option>
          </select>
        </div>
        <div class="form-field">
          <label for="set-smtp-from">From address</label>
          <input
            id="set-smtp-from"
            type="text"
            placeholder="vigil@example.com"
            value={from()}
            onInput={(e) => setFrom(e.currentTarget.value)}
          />
        </div>
        <div class="form-field">
          <label for="set-smtp-to">Recipients (comma-separated)</label>
          <input
            id="set-smtp-to"
            type="text"
            placeholder="you@example.com, oncall@example.com"
            value={recipients()}
            onInput={(e) => setRecipients(e.currentTarget.value)}
          />
        </div>

        <div class="detail-actions">
          <button type="button" class="btn-accent" disabled={saving()} onClick={handleSaveSmtp}>
            {saving() ? "Saving…" : "Save"}
          </button>
          <button
            type="button"
            class="btn-ghost"
            disabled={testing() || emailChannel() == null}
            onClick={handleSendTest}
          >
            {testing() ? "Sending…" : "Send test"}
          </button>
        </div>

        <Show when={saveNote()}>
          <div class="test-result mono">{saveNote()}</div>
        </Show>
        <Show when={testResult()}>
          <div class={`test-result mono ${testResult()?.ok ? "" : "test-result-error"}`}>
            {testResult()?.ok ? "Test email sent." : `Failed: ${testResult()?.error ?? "unknown error"}`}
          </div>
        </Show>
      </section>

      <section class="form-section settings-section">
        <h3 class="form-section-title">Anchor hosts</h3>
        <p class="settings-note">
          Known-good hosts used for the internet-sanity check before declaring any monitor DOWN.
        </p>
        <div class="form-field">
          <label for="set-anchors">Anchor hosts (comma or newline separated)</label>
          <textarea
            id="set-anchors"
            rows={3}
            placeholder="1.1.1.1:443, 8.8.8.8:443"
            value={anchorsText()}
            onInput={(e) => {
              setAnchorsText(e.currentTarget.value);
              setAnchorsSaved(false);
            }}
          />
        </div>
        <div class="detail-actions">
          <button type="button" class="btn-accent" disabled={anchorsSaving()} onClick={handleSaveAnchors}>
            {anchorsSaving() ? "Saving…" : "Save"}
          </button>
        </div>
        <Show when={anchorsSaved()}>
          <div class="test-result mono">Saved.</div>
        </Show>
      </section>

      <section class="form-section settings-section">
        <h3 class="form-section-title">Data retention</h3>
        <div class="form-field">
          <label for="set-retention">Raw check retention (days)</label>
          <input
            id="set-retention"
            type="number"
            min={1}
            value={retentionDays()}
            onInput={(e) => {
              setRetentionDays(Number(e.currentTarget.value) || DEFAULT_RETENTION_DAYS);
              setRetentionSaved(false);
            }}
          />
        </div>
        <div class="detail-actions">
          <button
            type="button"
            class="btn-accent"
            disabled={retentionSaving()}
            onClick={handleSaveRetention}
          >
            {retentionSaving() ? "Saving…" : "Save"}
          </button>
        </div>
        <Show when={retentionSaved()}>
          <div class="test-result mono">Saved.</div>
        </Show>
      </section>
    </div>
  );
};

export default Settings;
