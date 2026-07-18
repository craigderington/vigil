import { createSignal, For, onCleanup, onMount, Show, type Component } from "solid-js";

/**
 * Right-side Add/Edit monitor panel (Task 17). `api` is injected as a prop
 * (rather than imported directly) so tests can pass a bare
 * `{ testCheck }` stub — every call the form makes to `api` MUST therefore
 * be guarded with optional chaining (`api.getChannels?.()` etc.) so the
 * form degrades gracefully instead of crashing when a method is missing.
 */
export interface MonitorFormProps {
  api: any;
  monitor?: any;
  onSaved: () => void;
  onClose: () => void;
}

const MONITOR_TYPES: { label: string; value: string }[] = [
  { label: "HTTP", value: "http" },
  { label: "Keyword", value: "keyword" },
  { label: "Port", value: "port" },
  { label: "Ping", value: "ping" },
  { label: "DNS", value: "dns" },
];

const DNS_RECORD_TYPES = ["A", "AAAA", "CNAME", "MX", "TXT", "NS"];

const INTERVAL_PRESETS: { label: string; seconds: number }[] = [
  { label: "30s", seconds: 30 },
  { label: "1m", seconds: 60 },
  { label: "2m", seconds: 120 },
  { label: "5m", seconds: 300 },
  { label: "10m", seconds: 600 },
  { label: "15m", seconds: 900 },
  { label: "30m", seconds: 1800 },
  { label: "1h", seconds: 3600 },
  { label: "6h", seconds: 21600 },
  { label: "12h", seconds: 43200 },
  { label: "24h", seconds: 86400 },
];

type HeaderRow = { key: string; value: string };

function parseInitialHeaders(headers: string | null | undefined): HeaderRow[] {
  if (!headers) return [{ key: "", value: "" }];
  try {
    const obj = JSON.parse(headers);
    const rows = Object.entries(obj).map(([key, value]) => ({ key, value: String(value) }));
    return rows.length > 0 ? rows : [{ key: "", value: "" }];
  } catch {
    return [{ key: "", value: "" }];
  }
}

function parseInitialAuthValue(authRef: string | null | undefined): string {
  if (!authRef) return "";
  if (authRef.startsWith("inline:")) return authRef.slice("inline:".length);
  return authRef; // env:VAR — leave the prefix visible so it round-trips
}

type NotifRow = { attached: boolean; down: boolean; recovered: boolean };

const MonitorForm: Component<MonitorFormProps> = (props) => {
  const isEdit = () => props.monitor != null;

  const [name, setName] = createSignal(props.monitor?.name ?? "");
  const [type, setType] = createSignal<string>(props.monitor?.type ?? "http");
  const [url, setUrl] = createSignal(props.monitor?.url ?? "");
  const [method, setMethod] = createSignal(props.monitor?.method ?? "GET");

  // Type-specific fields (keyword / port / ping / dns) — §3 of the spec.
  const [host, setHost] = createSignal(props.monitor?.host ?? "");
  const [portValue, setPortValue] = createSignal<number | null>(props.monitor?.port ?? null);
  const [keyword, setKeyword] = createSignal(props.monitor?.keyword ?? "");
  const [keywordMode, setKeywordMode] = createSignal<string>(props.monitor?.keyword_mode ?? "present");
  const [keywordCaseSensitive, setKeywordCaseSensitive] = createSignal<boolean>(
    props.monitor?.keyword_case_sensitive ?? false,
  );
  const [dnsRecordType, setDnsRecordType] = createSignal<string>(props.monitor?.dns_record_type ?? "A");
  const [dnsExpectedValue, setDnsExpectedValue] = createSignal(props.monitor?.dns_expected_value ?? "");

  const isHttpLike = () => type() === "http" || type() === "keyword";

  const [intervalSeconds, setIntervalSeconds] = createSignal<number>(
    props.monitor?.interval_seconds ?? 300,
  );
  const [timeoutSeconds, setTimeoutSeconds] = createSignal<number>(
    props.monitor?.timeout_seconds ?? 30,
  );
  const [confirmationThreshold, setConfirmationThreshold] = createSignal<number>(
    props.monitor?.confirmation_threshold ?? 3,
  );
  const [recoveryThreshold, setRecoveryThreshold] = createSignal<number>(
    props.monitor?.recovery_threshold ?? 1,
  );
  const [retryInterval, setRetryInterval] = createSignal<number>(
    props.monitor?.retry_interval_seconds ?? 30,
  );

  const [expectedCodes, setExpectedCodes] = createSignal(
    props.monitor?.expected_status_codes ?? "200-299",
  );
  const [followRedirects, setFollowRedirects] = createSignal<boolean>(
    props.monitor?.follow_redirects ?? true,
  );
  const [verifySsl, setVerifySsl] = createSignal<boolean>(props.monitor?.verify_ssl ?? true);

  const [headerRows, setHeaderRows] = createSignal<HeaderRow[]>(
    parseInitialHeaders(props.monitor?.headers),
  );
  const [bodyText, setBodyText] = createSignal(props.monitor?.body ?? "");
  const [authType, setAuthType] = createSignal<string>(props.monitor?.auth_type ?? "none");
  const [authValue, setAuthValue] = createSignal(parseInitialAuthValue(props.monitor?.auth_ref));

  const [channels, setChannels] = createSignal<any[]>([]);
  const [notifState, setNotifState] = createSignal<Record<number, NotifRow>>({});

  const [testing, setTesting] = createSignal(false);
  const [testResult, setTestResult] = createSignal<any>(null);
  const [saving, setSaving] = createSignal(false);
  const [saveError, setSaveError] = createSignal<string | null>(null);

  function onKeyDown(e: KeyboardEvent) {
    if (e.key === "Escape") props.onClose();
  }
  onMount(() => document.addEventListener("keydown", onKeyDown));
  onCleanup(() => document.removeEventListener("keydown", onKeyDown));

  onMount(() => {
    (async () => {
      try {
        const list = await props.api.getChannels?.();
        if (!list) return;
        const emailChannels = list.filter((c: any) => c.type === "email");
        setChannels(emailChannels);

        const initial: Record<number, NotifRow> = {};
        for (const c of emailChannels) {
          initial[c.id] = { attached: false, down: true, recovered: true };
        }

        if (props.monitor?.id != null) {
          const existing = await props.api.getMonitorNotifications?.(props.monitor.id);
          for (const item of existing ?? []) {
            initial[item.channel_id] = {
              attached: true,
              down: item.triggers.includes("down"),
              recovered: item.triggers.includes("recovered"),
            };
          }
        }
        setNotifState(initial);
      } catch (e) {
        console.warn("MonitorForm: failed to load notification channels", e);
      }
    })();
  });

  function updateHeaderRow(i: number, field: "key" | "value", val: string) {
    setHeaderRows((rows) => rows.map((r, idx) => (idx === i ? { ...r, [field]: val } : r)));
  }
  function addHeaderRow() {
    setHeaderRows((rows) => [...rows, { key: "", value: "" }]);
  }
  function removeHeaderRow(i: number) {
    setHeaderRows((rows) => rows.filter((_, idx) => idx !== i));
  }

  function toggleAttached(channelId: number) {
    setNotifState((s) => ({
      ...s,
      [channelId]: { ...(s[channelId] ?? { attached: false, down: true, recovered: true }), attached: !s[channelId]?.attached },
    }));
  }
  function toggleTrigger(channelId: number, trigger: "down" | "recovered") {
    setNotifState((s) => ({
      ...s,
      [channelId]: { ...(s[channelId] ?? { attached: true, down: true, recovered: true }), [trigger]: !s[channelId]?.[trigger] },
    }));
  }

  function selectedNotifications(): { channel_id: number; triggers: string[] }[] {
    return Object.entries(notifState())
      .filter(([, v]) => v.attached)
      .map(([id, v]) => ({
        channel_id: Number(id),
        triggers: [...(v.down ? ["down"] : []), ...(v.recovered ? ["recovered"] : [])],
      }));
  }

  function buildDto() {
    const t = type();

    // Fields common to every monitor type (name + schedule). Type-specific
    // fields are layered on below so the DTO only carries what's relevant
    // to `t` — the backend's per-type validation (§5) expects exactly that.
    const dto: Record<string, unknown> = {
      name: name(),
      type: t,
      interval_seconds: Math.max(15, intervalSeconds()),
      timeout_seconds: Math.max(1, timeoutSeconds()),
      confirmation_threshold: Math.max(1, confirmationThreshold()),
      recovery_threshold: Math.max(1, recoveryThreshold()),
      retry_interval_seconds: Math.max(1, retryInterval()),
    };

    if (isHttpLike()) {
      const rows = headerRows().filter((r) => r.key.trim() !== "");
      const headers =
        rows.length > 0 ? JSON.stringify(Object.fromEntries(rows.map((r) => [r.key, r.value]))) : null;

      let authRef: string | null = null;
      if (authType() !== "none" && authValue().trim() !== "") {
        authRef = authValue().startsWith("env:") ? authValue() : `inline:${authValue()}`;
      }

      dto.url = url();
      dto.method = method();
      dto.headers = headers;
      dto.body = bodyText().trim() === "" ? null : bodyText();
      dto.auth_type = authType() === "none" ? null : authType();
      dto.auth_ref = authRef;
      dto.expected_status_codes = expectedCodes();
      dto.follow_redirects = followRedirects();
      dto.verify_ssl = verifySsl();

      if (t === "keyword") {
        dto.keyword = keyword();
        dto.keyword_mode = keywordMode();
        dto.keyword_case_sensitive = keywordCaseSensitive();
      }
    } else if (t === "port") {
      dto.host = host();
      dto.port = portValue();
    } else if (t === "ping") {
      dto.host = host();
      dto.port = portValue();
    } else if (t === "dns") {
      dto.host = host();
      dto.dns_record_type = dnsRecordType();
      dto.dns_expected_value = dnsExpectedValue().trim() === "" ? null : dnsExpectedValue();
    }

    return dto;
  }

  async function handleTestCheck() {
    setTesting(true);
    try {
      const result = await props.api.testCheck?.(buildDto());
      setTestResult(result ?? null);
    } finally {
      setTesting(false);
    }
  }

  async function handleSave() {
    setSaving(true);
    setSaveError(null);
    try {
      const dto = buildDto();
      let saved: any;
      if (isEdit()) {
        saved = await props.api.updateMonitor?.(props.monitor.id, dto);
      } else {
        saved = await props.api.createMonitor?.(dto);
      }
      const id = saved?.id ?? props.monitor?.id;
      if (id != null) {
        await props.api.setMonitorNotifications?.(id, selectedNotifications());
      }
      props.onSaved();
    } catch (e: any) {
      setSaveError(e?.message ?? "Failed to save monitor");
    } finally {
      setSaving(false);
    }
  }

  return (
    <div class="detail-backdrop" onClick={props.onClose}>
      <div
        class="detail-panel"
        role="dialog"
        aria-modal="true"
        aria-label={isEdit() ? "Edit monitor" : "Add monitor"}
        onClick={(e) => e.stopPropagation()}
      >
        <div class="detail-header">
          <div class="detail-header-top">
            <h2 class="detail-name">{isEdit() ? "Edit monitor" : "Add monitor"}</h2>
            <button type="button" class="detail-close" aria-label="Close" onClick={props.onClose}>
              &#10005;
            </button>
          </div>
        </div>

        <div class="detail-body form-body">
          <section class="form-section">
            <h3 class="form-section-title">Basics</h3>
            <div class="form-field">
              <label for="mf-name">Name</label>
              <input
                id="mf-name"
                type="text"
                value={name()}
                onInput={(e) => setName(e.currentTarget.value)}
              />
            </div>
            <div class="form-field">
              <label>Type</label>
              <div class="chip-row" role="group" aria-label="Monitor type">
                <For each={MONITOR_TYPES}>
                  {(opt) => (
                    <button
                      type="button"
                      class="chip"
                      aria-pressed={type() === opt.value}
                      disabled={isEdit()}
                      onClick={() => setType(opt.value)}
                    >
                      {opt.label}
                    </button>
                  )}
                </For>
              </div>
            </div>

            <Show when={isHttpLike()}>
              <div class="form-field">
                <label for="mf-url">URL</label>
                <input
                  id="mf-url"
                  type="text"
                  placeholder="https://example.com/health"
                  value={url()}
                  onInput={(e) => setUrl(e.currentTarget.value)}
                />
              </div>
            </Show>

            <Show when={type() === "keyword"}>
              <div class="form-field">
                <label for="mf-keyword">Keyword</label>
                <input
                  id="mf-keyword"
                  type="text"
                  value={keyword()}
                  onInput={(e) => setKeyword(e.currentTarget.value)}
                />
              </div>
              <div class="form-field">
                <label for="mf-keyword-mode">Keyword mode</label>
                <select
                  id="mf-keyword-mode"
                  value={keywordMode()}
                  onChange={(e) => setKeywordMode(e.currentTarget.value)}
                >
                  <option value="present">Present</option>
                  <option value="absent">Absent</option>
                </select>
              </div>
              <label class="form-checkbox">
                <input
                  type="checkbox"
                  checked={keywordCaseSensitive()}
                  onChange={(e) => setKeywordCaseSensitive(e.currentTarget.checked)}
                />
                Case sensitive
              </label>
            </Show>

            <Show when={type() === "port" || type() === "ping" || type() === "dns"}>
              <div class="form-field">
                <label for="mf-host">Host</label>
                <input
                  id="mf-host"
                  type="text"
                  placeholder="example.com"
                  value={host()}
                  onInput={(e) => setHost(e.currentTarget.value)}
                />
              </div>
            </Show>

            <Show when={type() === "port"}>
              <div class="form-field">
                <label for="mf-port">Port</label>
                <input
                  id="mf-port"
                  type="number"
                  min={1}
                  max={65535}
                  value={portValue() ?? ""}
                  onInput={(e) =>
                    setPortValue(e.currentTarget.value === "" ? null : Number(e.currentTarget.value))
                  }
                />
              </div>
            </Show>

            <Show when={type() === "ping"}>
              <div class="form-field">
                <label for="mf-port">Port</label>
                <input
                  id="mf-port"
                  type="number"
                  min={1}
                  max={65535}
                  placeholder="443, 80 fallback"
                  value={portValue() ?? ""}
                  onInput={(e) =>
                    setPortValue(e.currentTarget.value === "" ? null : Number(e.currentTarget.value))
                  }
                />
              </div>
            </Show>

            <Show when={type() === "dns"}>
              <div class="form-field">
                <label for="mf-dns-record-type">Record type</label>
                <select
                  id="mf-dns-record-type"
                  value={dnsRecordType()}
                  onChange={(e) => setDnsRecordType(e.currentTarget.value)}
                >
                  <For each={DNS_RECORD_TYPES}>{(rt) => <option value={rt}>{rt}</option>}</For>
                </select>
              </div>
              <div class="form-field">
                <label for="mf-dns-expected-value">Expected value (optional)</label>
                <input
                  id="mf-dns-expected-value"
                  type="text"
                  value={dnsExpectedValue()}
                  onInput={(e) => setDnsExpectedValue(e.currentTarget.value)}
                />
              </div>
            </Show>
          </section>

          <section class="form-section">
            <h3 class="form-section-title">Schedule</h3>
            <div class="form-field">
              <label>Interval</label>
              <div class="chip-row" role="group" aria-label="Interval preset">
                <For each={INTERVAL_PRESETS}>
                  {(preset) => (
                    <button
                      type="button"
                      class="chip"
                      aria-pressed={intervalSeconds() === preset.seconds}
                      onClick={() => setIntervalSeconds(preset.seconds)}
                    >
                      {preset.label}
                    </button>
                  )}
                </For>
              </div>
            </div>
            <div class="form-field">
              <label for="mf-interval-custom">Custom interval (seconds, min 15)</label>
              <input
                id="mf-interval-custom"
                type="number"
                min={15}
                value={intervalSeconds()}
                onInput={(e) => setIntervalSeconds(Math.max(15, Number(e.currentTarget.value) || 15))}
              />
            </div>
            <div class="form-field">
              <label for="mf-timeout">Timeout (seconds)</label>
              <input
                id="mf-timeout"
                type="number"
                min={1}
                value={timeoutSeconds()}
                onInput={(e) => setTimeoutSeconds(Number(e.currentTarget.value) || 1)}
              />
            </div>
            <div class="form-field">
              <label for="mf-confirmation">Confirmation threshold</label>
              <input
                id="mf-confirmation"
                type="number"
                min={1}
                value={confirmationThreshold()}
                onInput={(e) => setConfirmationThreshold(Number(e.currentTarget.value) || 1)}
              />
            </div>
            <div class="form-field">
              <label for="mf-recovery">Recovery threshold</label>
              <input
                id="mf-recovery"
                type="number"
                min={1}
                value={recoveryThreshold()}
                onInput={(e) => setRecoveryThreshold(Number(e.currentTarget.value) || 1)}
              />
            </div>
            <div class="form-field">
              <label for="mf-retry">Retry interval (seconds)</label>
              <input
                id="mf-retry"
                type="number"
                min={1}
                value={retryInterval()}
                onInput={(e) => setRetryInterval(Number(e.currentTarget.value) || 1)}
              />
            </div>
          </section>

          <Show when={isHttpLike()}>
            <section class="form-section">
              <h3 class="form-section-title">Validation</h3>
              <div class="form-field">
                <label for="mf-codes">Expected status codes</label>
                <input
                  id="mf-codes"
                  type="text"
                  value={expectedCodes()}
                  onInput={(e) => setExpectedCodes(e.currentTarget.value)}
                />
              </div>
              <label class="form-checkbox">
                <input
                  type="checkbox"
                  checked={followRedirects()}
                  onChange={(e) => setFollowRedirects(e.currentTarget.checked)}
                />
                Follow redirects
              </label>
              <label class="form-checkbox">
                <input
                  type="checkbox"
                  checked={verifySsl()}
                  onChange={(e) => setVerifySsl(e.currentTarget.checked)}
                />
                Verify SSL
              </label>
            </section>

            <section class="form-section">
              <h3 class="form-section-title">Advanced</h3>
              <div class="form-field">
                <label for="mf-method">Method</label>
                <select id="mf-method" value={method()} onChange={(e) => setMethod(e.currentTarget.value)}>
                  <option value="GET">GET</option>
                  <option value="POST">POST</option>
                  <option value="HEAD">HEAD</option>
                </select>
              </div>

              <div class="form-field">
                <label>Request headers</label>
                <For each={headerRows()}>
                  {(row, i) => (
                    <div class="header-row">
                      <input
                        type="text"
                        placeholder="Header"
                        value={row.key}
                        onInput={(e) => updateHeaderRow(i(), "key", e.currentTarget.value)}
                      />
                      <input
                        type="text"
                        placeholder="Value"
                        value={row.value}
                        onInput={(e) => updateHeaderRow(i(), "value", e.currentTarget.value)}
                      />
                      <button type="button" class="btn-link" onClick={() => removeHeaderRow(i())}>
                        Remove
                      </button>
                    </div>
                  )}
                </For>
                <button type="button" class="btn-link" onClick={addHeaderRow}>
                  + Add header
                </button>
              </div>

              <div class="form-field">
                <label for="mf-body">Body</label>
                <textarea id="mf-body" rows={3} value={bodyText()} onInput={(e) => setBodyText(e.currentTarget.value)} />
              </div>

              <div class="form-field">
                <label for="mf-auth-type">Auth</label>
                <select
                  id="mf-auth-type"
                  value={authType()}
                  onChange={(e) => setAuthType(e.currentTarget.value)}
                >
                  <option value="none">None</option>
                  <option value="basic">Basic</option>
                  <option value="bearer">Bearer</option>
                  <option value="header">Header</option>
                </select>
              </div>
              <Show when={authType() !== "none"}>
                <div class="form-field">
                  <label for="mf-auth-value">
                    Auth value (prefix with "env:" to reference an environment variable)
                  </label>
                  <input
                    id="mf-auth-value"
                    type="text"
                    value={authValue()}
                    onInput={(e) => setAuthValue(e.currentTarget.value)}
                  />
                </div>
              </Show>
            </section>
          </Show>

          <Show when={channels().length > 0}>
            <section class="form-section">
              <h3 class="form-section-title">Notifications</h3>
              <For each={channels()}>
                {(c) => {
                  const row = () => notifState()[c.id] ?? { attached: false, down: true, recovered: true };
                  return (
                    <div class="notif-row">
                      <label class="form-checkbox">
                        <input type="checkbox" checked={row().attached} onChange={() => toggleAttached(c.id)} />
                        {c.name}
                      </label>
                      <Show when={row().attached}>
                        <label class="form-checkbox inline">
                          <input
                            type="checkbox"
                            checked={row().down}
                            onChange={() => toggleTrigger(c.id, "down")}
                          />
                          down
                        </label>
                        <label class="form-checkbox inline">
                          <input
                            type="checkbox"
                            checked={row().recovered}
                            onChange={() => toggleTrigger(c.id, "recovered")}
                          />
                          recovered
                        </label>
                      </Show>
                    </div>
                  );
                }}
              </For>
            </section>
          </Show>

          <div class="detail-actions">
            <button type="button" class="btn-ghost" disabled={testing()} onClick={handleTestCheck}>
              Test check
            </button>
            <button type="button" class="btn-accent" disabled={saving()} onClick={handleSave}>
              {isEdit() ? "Save changes" : "Create monitor"}
            </button>
            <button type="button" class="btn-ghost" onClick={props.onClose}>
              Close
            </button>
          </div>

          <Show when={testResult()}>
            <div class="test-result mono">
              {testResult()?.ok ? "OK" : "Failed"} · status {testResult()?.status_code ?? "—"} ·{" "}
              {testResult()?.response_time_ms ?? "—"}ms
              <Show when={testResult()?.error_message}>
                <div class="test-result-error">{testResult()?.error_message}</div>
              </Show>
            </div>
          </Show>

          <Show when={saveError()}>
            <div class="test-result-error">{saveError()}</div>
          </Show>
        </div>
      </div>
    </div>
  );
};

export default MonitorForm;
