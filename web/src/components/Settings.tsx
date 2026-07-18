import { createSignal, For, onMount, Show, type Component } from "solid-js";
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

const CHANNEL_TYPES: { label: string; value: string }[] = [
  { label: "Email", value: "email" },
  { label: "Webhook", value: "webhook" },
  { label: "Discord", value: "discord" },
  { label: "ntfy", value: "ntfy" },
];

const DEFAULT_CHANNEL_NAMES: Record<string, string> = {
  email: "Email",
  webhook: "Webhook",
  discord: "Discord",
  ntfy: "ntfy",
};

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

  // Multi-type channel manager (Task 12): a list of every channel (any
  // type) plus an "Add channel" form with a type selector and type-specific
  // config fields. The single-email SMTP section above is untouched — this
  // is additive, for webhook/discord/ntfy (and extra email) channels.
  const [channels, setChannels] = createSignal<any[]>([]);
  const [channelTestState, setChannelTestState] = createSignal<
    Record<number, { testing: boolean; result: { ok: boolean; error?: string | null } | null }>
  >({});

  const [newType, setNewType] = createSignal<string>("email");
  const [newName, setNewName] = createSignal("");

  const [newEmailHost, setNewEmailHost] = createSignal("");
  const [newEmailPort, setNewEmailPort] = createSignal<number>(587);
  const [newEmailSecurity, setNewEmailSecurity] = createSignal("starttls");
  const [newEmailFrom, setNewEmailFrom] = createSignal("");
  const [newEmailTo, setNewEmailTo] = createSignal("");
  const [newEmailUsername, setNewEmailUsername] = createSignal("");

  const [newWebhookUrl, setNewWebhookUrl] = createSignal("");
  const [newWebhookMethod, setNewWebhookMethod] = createSignal("POST");
  const [newWebhookHeaders, setNewWebhookHeaders] = createSignal("");
  const [newWebhookTemplate, setNewWebhookTemplate] = createSignal("");

  const [newDiscordUrl, setNewDiscordUrl] = createSignal("");

  const [newNtfyServer, setNewNtfyServer] = createSignal("https://ntfy.sh");
  const [newNtfyTopic, setNewNtfyTopic] = createSignal("");
  const [newNtfyPriority, setNewNtfyPriority] = createSignal("");
  const [newNtfyToken, setNewNtfyToken] = createSignal("");

  const [addSaving, setAddSaving] = createSignal(false);
  const [addNote, setAddNote] = createSignal<string | null>(null);

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
      setChannels(arr);
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
      // Backend stores `config` verbatim as a JSON-object string (it no longer
      // re-encodes it), so the wire contract requires an already-serialized
      // string here, not a nested object.
      const config = JSON.stringify(buildEmailConfig());
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

  function buildTypeConfig(t: string): Record<string, unknown> {
    switch (t) {
      case "email": {
        const cfg: Record<string, unknown> = {
          host: newEmailHost(),
          port: Number(newEmailPort()) || 0,
          security: newEmailSecurity(),
          from: newEmailFrom(),
          to: newEmailTo()
            .split(",")
            .map((s) => s.trim())
            .filter((s) => s.length > 0),
        };
        if (newEmailUsername().trim() !== "") cfg.username = newEmailUsername().trim();
        return cfg;
      }
      case "webhook": {
        const cfg: Record<string, unknown> = { url: newWebhookUrl(), method: newWebhookMethod() };
        if (newWebhookHeaders().trim() !== "") {
          try {
            cfg.headers = JSON.parse(newWebhookHeaders());
          } catch {
            // invalid JSON — omit rather than send a broken config
          }
        }
        if (newWebhookTemplate().trim() !== "") cfg.template = newWebhookTemplate();
        return cfg;
      }
      case "discord":
        return { webhook_url: newDiscordUrl() };
      case "ntfy": {
        const cfg: Record<string, unknown> = {
          server: newNtfyServer().trim() || "https://ntfy.sh",
          topic: newNtfyTopic(),
        };
        if (newNtfyPriority().trim() !== "") cfg.priority = newNtfyPriority().trim();
        if (newNtfyToken().trim() !== "") cfg.token = newNtfyToken().trim();
        return cfg;
      }
      default:
        return {};
    }
  }

  function resetAddForm() {
    setNewName("");
    setNewEmailHost("");
    setNewEmailPort(587);
    setNewEmailSecurity("starttls");
    setNewEmailFrom("");
    setNewEmailTo("");
    setNewEmailUsername("");
    setNewWebhookUrl("");
    setNewWebhookMethod("POST");
    setNewWebhookHeaders("");
    setNewWebhookTemplate("");
    setNewDiscordUrl("");
    setNewNtfyServer("https://ntfy.sh");
    setNewNtfyTopic("");
    setNewNtfyPriority("");
    setNewNtfyToken("");
  }

  async function handleAddChannel() {
    setAddSaving(true);
    setAddNote(null);
    try {
      const t = newType();
      const name = newName().trim() || DEFAULT_CHANNEL_NAMES[t] || t;
      const config = JSON.stringify(buildTypeConfig(t));
      const saved = await api.createChannel({ name, type: t, config });
      if (saved?.id != null) {
        setChannels((cs) => [...cs, saved]);
        if (t === "email" && emailChannel() == null) {
          setEmailChannel(saved);
          loadChannelIntoForm(saved);
        }
      }
      resetAddForm();
      setAddNote("Channel added.");
    } catch (e: any) {
      setAddNote(e?.message ?? "Failed to add channel.");
    } finally {
      setAddSaving(false);
    }
  }

  async function handleTestExistingChannel(id: number) {
    setChannelTestState((s) => ({ ...s, [id]: { testing: true, result: null } }));
    try {
      const result = await api.testChannel(id);
      setChannelTestState((s) => ({ ...s, [id]: { testing: false, result } }));
    } catch (e: any) {
      setChannelTestState((s) => ({
        ...s,
        [id]: { testing: false, result: { ok: false, error: e?.message ?? "Failed to send test." } },
      }));
    }
  }

  async function handleDeleteChannel(id: number) {
    try {
      await api.deleteChannel(id);
      setChannels((cs) => cs.filter((c) => c.id !== id));
      if (emailChannel()?.id === id) setEmailChannel(null);
    } catch {
      // leave the channel in the list so the operator can retry
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
        <h3 class="form-section-title">Notification channels</h3>
        <p class="settings-note">
          Email, webhook, Discord, and ntfy channels for down/recovered and certificate alerts.
        </p>
        <Show when={channels().length === 0}>
          <p class="settings-note">No channels yet.</p>
        </Show>
        <For each={channels()}>
          {(c) => {
            const state = () => channelTestState()[c.id] ?? { testing: false, result: null };
            return (
              <div class="notif-row">
                <span>
                  {c.name} <span class="settings-note">({c.type})</span>
                </span>
                <button
                  type="button"
                  class="btn-link"
                  disabled={state().testing}
                  onClick={() => handleTestExistingChannel(c.id)}
                >
                  {state().testing ? "Sending…" : "Send test"}
                </button>
                <button type="button" class="btn-link danger" onClick={() => handleDeleteChannel(c.id)}>
                  Delete
                </button>
                <Show when={state().result}>
                  <div class={`test-result mono ${state().result?.ok ? "" : "test-result-error"}`}>
                    {state().result?.ok ? "Test sent." : `Failed: ${state().result?.error ?? "unknown error"}`}
                  </div>
                </Show>
              </div>
            );
          }}
        </For>
      </section>

      <section class="form-section settings-section">
        <h3 class="form-section-title">Add channel</h3>
        <div class="form-field">
          <label for="ch-name">Channel name</label>
          <input
            id="ch-name"
            type="text"
            placeholder={DEFAULT_CHANNEL_NAMES[newType()]}
            value={newName()}
            onInput={(e) => setNewName(e.currentTarget.value)}
          />
        </div>
        <div class="form-field">
          <label>Channel type</label>
          <div class="chip-row" role="group" aria-label="Channel type">
            <For each={CHANNEL_TYPES}>
              {(opt) => (
                <button
                  type="button"
                  class="chip"
                  aria-pressed={newType() === opt.value}
                  onClick={() => setNewType(opt.value)}
                >
                  {opt.label}
                </button>
              )}
            </For>
          </div>
        </div>

        <Show when={newType() === "email"}>
          <div class="form-field">
            <label for="ch-email-host">SMTP host</label>
            <input
              id="ch-email-host"
              type="text"
              placeholder="smtp.example.com"
              value={newEmailHost()}
              onInput={(e) => setNewEmailHost(e.currentTarget.value)}
            />
          </div>
          <div class="form-field">
            <label for="ch-email-port">SMTP port</label>
            <input
              id="ch-email-port"
              type="number"
              min={1}
              value={newEmailPort()}
              onInput={(e) => setNewEmailPort(Number(e.currentTarget.value) || 0)}
            />
          </div>
          <div class="form-field">
            <label for="ch-email-security">SMTP security</label>
            <select
              id="ch-email-security"
              value={newEmailSecurity()}
              onChange={(e) => setNewEmailSecurity(e.currentTarget.value)}
            >
              <option value="none">None</option>
              <option value="starttls">STARTTLS</option>
              <option value="tls">TLS</option>
            </select>
          </div>
          <div class="form-field">
            <label for="ch-email-from">SMTP from address</label>
            <input
              id="ch-email-from"
              type="text"
              placeholder="vigil@example.com"
              value={newEmailFrom()}
              onInput={(e) => setNewEmailFrom(e.currentTarget.value)}
            />
          </div>
          <div class="form-field">
            <label for="ch-email-to">SMTP recipients (comma-separated)</label>
            <input
              id="ch-email-to"
              type="text"
              placeholder="you@example.com, oncall@example.com"
              value={newEmailTo()}
              onInput={(e) => setNewEmailTo(e.currentTarget.value)}
            />
          </div>
          <div class="form-field">
            <label for="ch-email-username">SMTP username (optional)</label>
            <input
              id="ch-email-username"
              type="text"
              placeholder="defaults to From address"
              value={newEmailUsername()}
              onInput={(e) => setNewEmailUsername(e.currentTarget.value)}
            />
          </div>
        </Show>

        <Show when={newType() === "webhook"}>
          <div class="form-field">
            <label for="ch-webhook-url">URL</label>
            <input
              id="ch-webhook-url"
              type="text"
              placeholder="https://example.com/hook"
              value={newWebhookUrl()}
              onInput={(e) => setNewWebhookUrl(e.currentTarget.value)}
            />
          </div>
          <div class="form-field">
            <label for="ch-webhook-method">Method</label>
            <select
              id="ch-webhook-method"
              value={newWebhookMethod()}
              onChange={(e) => setNewWebhookMethod(e.currentTarget.value)}
            >
              <option value="POST">POST</option>
              <option value="PUT">PUT</option>
              <option value="GET">GET</option>
            </select>
          </div>
          <div class="form-field">
            <label for="ch-webhook-headers">Headers (JSON, optional)</label>
            <textarea
              id="ch-webhook-headers"
              rows={2}
              placeholder='{"Authorization":"Bearer ..."}'
              value={newWebhookHeaders()}
              onInput={(e) => setNewWebhookHeaders(e.currentTarget.value)}
            />
          </div>
          <div class="form-field">
            <label for="ch-webhook-template">Body template (optional)</label>
            <textarea
              id="ch-webhook-template"
              rows={2}
              placeholder="{{monitor_name}} is {{status}}"
              value={newWebhookTemplate()}
              onInput={(e) => setNewWebhookTemplate(e.currentTarget.value)}
            />
          </div>
        </Show>

        <Show when={newType() === "discord"}>
          <div class="form-field">
            <label for="ch-discord-url">Discord webhook URL</label>
            <input
              id="ch-discord-url"
              type="text"
              placeholder="https://discord.com/api/webhooks/..."
              value={newDiscordUrl()}
              onInput={(e) => setNewDiscordUrl(e.currentTarget.value)}
            />
          </div>
        </Show>

        <Show when={newType() === "ntfy"}>
          <div class="form-field">
            <label for="ch-ntfy-server">ntfy server</label>
            <input
              id="ch-ntfy-server"
              type="text"
              placeholder="https://ntfy.sh"
              value={newNtfyServer()}
              onInput={(e) => setNewNtfyServer(e.currentTarget.value)}
            />
          </div>
          <div class="form-field">
            <label for="ch-ntfy-topic">Topic</label>
            <input
              id="ch-ntfy-topic"
              type="text"
              value={newNtfyTopic()}
              onInput={(e) => setNewNtfyTopic(e.currentTarget.value)}
            />
          </div>
          <div class="form-field">
            <label for="ch-ntfy-priority">Priority (optional)</label>
            <input
              id="ch-ntfy-priority"
              type="text"
              placeholder="default, high, urgent…"
              value={newNtfyPriority()}
              onInput={(e) => setNewNtfyPriority(e.currentTarget.value)}
            />
          </div>
          <div class="form-field">
            <label for="ch-ntfy-token">Token (optional)</label>
            <input
              id="ch-ntfy-token"
              type="text"
              value={newNtfyToken()}
              onInput={(e) => setNewNtfyToken(e.currentTarget.value)}
            />
          </div>
        </Show>

        <div class="detail-actions">
          <button type="button" class="btn-accent" disabled={addSaving()} onClick={handleAddChannel}>
            {addSaving() ? "Adding…" : "Create channel"}
          </button>
        </div>
        <Show when={addNote()}>
          <div class="test-result mono">{addNote()}</div>
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
