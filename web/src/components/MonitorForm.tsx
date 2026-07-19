import { createEffect, createSignal, For, onCleanup, onMount, Show, type Component } from "solid-js";
import { pingUrl } from "../api";
import type { HeartbeatInfo } from "../api";

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
  { label: "SSL", value: "ssl" },
  { label: "Heartbeat", value: "heartbeat" },
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

/** `ssl_alert_days`/`domain_alert_days` arrive from the API as a JSON-array
 *  STRING (e.g. `"[30,14,7,3,1]"`). The form edits them as a plain
 *  comma-separated text field — no chip widget, just a text input. */
function daysToText(json: string | null | undefined): string | null {
  if (!json) return null;
  try {
    const arr = JSON.parse(json);
    if (!Array.isArray(arr)) return null;
    return arr.join(",");
  } catch {
    return null;
  }
}

function textToDaysJson(text: string): string {
  const days = text
    .split(",")
    .map((x) => parseInt(x.trim(), 10))
    .filter((n) => !isNaN(n));
  return JSON.stringify(days);
}

type NotifRow = {
  attached: boolean;
  down: boolean;
  heartbeat_missed: boolean;
  recovered: boolean;
  ssl_expiring: boolean;
  ssl_invalid: boolean;
  domain_expiring: boolean;
};

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

  // Certificate & Domain (§6) — SSL is allowed on an https URL or the
  // dedicated ssl type; domain expiry just needs a host/url to derive a
  // registrable domain from, so it's kept simple and always available.
  const [sslCheckEnabled, setSslCheckEnabled] = createSignal<boolean>(
    props.monitor?.ssl_check_enabled ?? false,
  );
  const [sslAlertDays, setSslAlertDays] = createSignal<string>(
    daysToText(props.monitor?.ssl_alert_days) ?? "30,14,7,3,1",
  );
  const [domainCheckEnabled, setDomainCheckEnabled] = createSignal<boolean>(
    props.monitor?.domain_check_enabled ?? false,
  );
  const [domainAlertDays, setDomainAlertDays] = createSignal<string>(
    daysToText(props.monitor?.domain_alert_days) ?? "45,30,14,7",
  );

  const sslAllowed = () => (isHttpLike() && url().startsWith("https://")) || type() === "ssl";

  // Defense-in-depth #2: if the monitor config drifts to SSL-ineligible
  // (type switched away from http/keyword/ssl, or the URL moved off
  // https://) while the toggle is still on, clear it so the checkbox
  // doesn't strand itself checked+disabled and the alert-days editor
  // doesn't linger under now-irrelevant fields. No-op for type==="ssl"
  // since sslAllowed() is always true there — doesn't fight the
  // forced-on rendering below.
  createEffect(() => {
    if (!sslAllowed() && sslCheckEnabled()) setSslCheckEnabled(false);
  });

  const [intervalSeconds, setIntervalSeconds] = createSignal<number>(
    props.monitor?.interval_seconds ?? 300,
  );
  // Global floor is 15s (§4), but the backend requires >=30s specifically
  // for heartbeat monitors (a push interval shorter than that isn't
  // meaningful). Read at call-time off `type()` so the custom-interval
  // input's `min`/clamp and the DTO's clamp both track a live type switch.
  const intervalFloor = () => (type() === "heartbeat" ? 30 : 15);
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

  // Heartbeat (P4 §3) — how long past `interval_seconds` a missed ping is
  // tolerated before the reaper drives the monitor DOWN. Confirmation/
  // recovery thresholds don't apply to a push monitor (backend forces them
  // to 1 regardless — §5), so their inputs are hidden for this type instead.
  const [graceSeconds, setGraceSeconds] = createSignal<number>(
    props.monitor?.heartbeat_grace_seconds ?? 60,
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

  // A freshly-attached channel defaults BOTH `down` and `heartbeat_missed`
  // on (along with `recovered`). This row is type-agnostic by design: which
  // of the two actually gets emitted is decided at save time in
  // `selectedNotifications()` based on the monitor's CURRENT type, not baked
  // in here based on whatever `type()` happened to be when the row was
  // created. That matters because channels typically finish loading (and
  // rows get created) before the user has clicked a type chip — if the
  // default depended on `type()` at creation time, a channel loaded while
  // still "http" would carry `down:true, heartbeat_missed:false` forever,
  // even after switching to Heartbeat, silently defeating the dead-man's
  // switch (a real monitor would emit "down" instead of "heartbeat_missed").
  function defaultNotifRow(attached: boolean): NotifRow {
    return {
      attached,
      down: true,
      heartbeat_missed: true,
      recovered: true,
      ssl_expiring: false,
      ssl_invalid: false,
      domain_expiring: false,
    };
  }

  const [testing, setTesting] = createSignal(false);
  const [testResult, setTestResult] = createSignal<any>(null);
  const [saving, setSaving] = createSignal(false);
  const [saveError, setSaveError] = createSignal<string | null>(null);
  // Set only right after creating a NEW heartbeat monitor — its ping URL
  // is shown in place of the normal footer actions until the user
  // dismisses it (`Done`), since the token is only ever available from
  // this one dedicated endpoint (never on the Monitor object itself).
  const [heartbeatInfo, setHeartbeatInfo] = createSignal<HeartbeatInfo | null>(null);

  // Once a new heartbeat's ping URL is showing, the monitor is already
  // saved — every dismissal path (✕, Escape, backdrop click, "Done") must
  // trigger onSaved() (store refresh) rather than a plain onClose(), same
  // as clicking "Done" does.
  function dismiss() {
    if (heartbeatInfo()) props.onSaved();
    else props.onClose();
  }

  function onKeyDown(e: KeyboardEvent) {
    if (e.key === "Escape") dismiss();
  }
  onMount(() => document.addEventListener("keydown", onKeyDown));
  onCleanup(() => document.removeEventListener("keydown", onKeyDown));

  onMount(() => {
    (async () => {
      try {
        const list = await props.api.getChannels?.();
        if (!list) return;
        // All channel types (email/webhook/discord/ntfy) can be attached —
        // certificate alerts (ssl_expiring/ssl_invalid/domain_expiring)
        // aren't email-only, so this is no longer filtered to type==="email".
        setChannels(list);

        const initial: Record<number, NotifRow> = {};
        for (const c of list) {
          initial[c.id] = defaultNotifRow(false);
        }

        if (props.monitor?.id != null) {
          const existing = await props.api.getMonitorNotifications?.(props.monitor.id);
          for (const item of existing ?? []) {
            initial[item.channel_id] = {
              attached: true,
              down: item.triggers.includes("down"),
              heartbeat_missed: item.triggers.includes("heartbeat_missed"),
              recovered: item.triggers.includes("recovered"),
              ssl_expiring: item.triggers.includes("ssl_expiring"),
              ssl_invalid: item.triggers.includes("ssl_invalid"),
              domain_expiring: item.triggers.includes("domain_expiring"),
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
      [channelId]: { ...(s[channelId] ?? defaultNotifRow(false)), attached: !s[channelId]?.attached },
    }));
  }
  function toggleTrigger(
    channelId: number,
    trigger: "down" | "heartbeat_missed" | "recovered" | "ssl_expiring" | "ssl_invalid" | "domain_expiring",
  ) {
    setNotifState((s) => ({
      ...s,
      [channelId]: { ...(s[channelId] ?? defaultNotifRow(true)), [trigger]: !s[channelId]?.[trigger] },
    }));
  }

  // Type-aware at EMIT time (not load time — see `defaultNotifRow` above):
  // a heartbeat monitor has no probe-driven "down" trigger, so `row.down`
  // (which defaults true and may be stale from before a type switch) must
  // never leak into a heartbeat's triggers, and `row.heartbeat_missed`
  // (also defaults true) is always emitted for it instead. Non-heartbeat
  // types never emit `heartbeat_missed` — that trigger doesn't apply there.
  function selectedNotifications(): { channel_id: number; triggers: string[] }[] {
    const isHeartbeat = type() === "heartbeat";
    return Object.entries(notifState())
      .filter(([, v]) => v.attached)
      .map(([id, v]) => ({
        channel_id: Number(id),
        triggers: isHeartbeat
          ? [
              ...(v.heartbeat_missed ? ["heartbeat_missed"] : []),
              ...(v.recovered ? ["recovered"] : []),
            ]
          : [
              ...(v.down ? ["down"] : []),
              ...(v.recovered ? ["recovered"] : []),
              ...(v.ssl_expiring ? ["ssl_expiring"] : []),
              ...(v.ssl_invalid ? ["ssl_invalid"] : []),
              ...(v.domain_expiring ? ["domain_expiring"] : []),
            ],
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
      interval_seconds: Math.max(intervalFloor(), intervalSeconds()),
      timeout_seconds: Math.max(1, timeoutSeconds()),
      confirmation_threshold: Math.max(1, confirmationThreshold()),
      recovery_threshold: Math.max(1, recoveryThreshold()),
      retry_interval_seconds: Math.max(1, retryInterval()),
    };

    // Heartbeat is a push monitor: no probe target, no confirmation cadence
    // (backend forces confirmation/recovery to 1 regardless — §5), and
    // ssl_check_enabled/domain_check_enabled are outright rejected by the
    // backend for this type, so they're never appended here (returning
    // early rather than sending explicit `false`s keeps the DTO minimal and
    // matches what the backend actually validates against).
    if (t === "heartbeat") {
      dto.heartbeat_grace_seconds = Math.max(1, graceSeconds());
      return dto;
    }

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
    } else if (t === "port" || t === "ssl") {
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

    // Certificate & Domain (§6) — always sent so edits can turn them off.
    // Clamped (not just read from the signal) so a stale/contradictory
    // ssl_check_enabled can never reach the backend even if the UI-clearing
    // effect above hasn't run yet for some reason.
    dto.ssl_check_enabled = t === "ssl" ? true : sslAllowed() && sslCheckEnabled();
    dto.ssl_alert_days = textToDaysJson(sslAlertDays());
    dto.domain_check_enabled = domainCheckEnabled();
    dto.domain_alert_days = textToDaysJson(domainAlertDays());

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

      // A brand-new heartbeat's ping URL only ever exists behind this one
      // dedicated endpoint (never on the Monitor object itself — §9/§10),
      // so this is the only chance the user gets to see/copy it without
      // hunting for it later in the detail panel's HeartbeatCard. Keep the
      // form open to show it instead of closing via `onSaved()`.
      if (!isEdit() && type() === "heartbeat" && id != null) {
        const hb = await props.api.getHeartbeat?.(id);
        if (hb) {
          setHeartbeatInfo(hb);
          return;
        }
      }

      props.onSaved();
    } catch (e: any) {
      setSaveError(e?.message ?? "Failed to save monitor");
    } finally {
      setSaving(false);
    }
  }

  async function handleCopyPingUrl() {
    const hb = heartbeatInfo();
    if (!hb) return;
    try {
      await navigator.clipboard.writeText(pingUrl(hb.ping_path));
    } catch {
      // clipboard API may be unavailable (e.g. insecure context) — no-op
    }
  }

  return (
    <div class="detail-backdrop" onClick={dismiss}>
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
            <button type="button" class="detail-close" aria-label="Close" onClick={dismiss}>
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

            <Show when={type() === "port" || type() === "ping" || type() === "dns" || type() === "ssl"}>
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

            <Show when={type() === "port" || type() === "ssl"}>
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
              <label for="mf-interval-custom">Custom interval (seconds, min {intervalFloor()})</label>
              <input
                id="mf-interval-custom"
                type="number"
                min={intervalFloor()}
                value={intervalSeconds()}
                onInput={(e) =>
                  setIntervalSeconds(Math.max(intervalFloor(), Number(e.currentTarget.value) || intervalFloor()))
                }
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
            <Show when={type() !== "heartbeat"}>
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
            </Show>
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
            <Show when={type() === "heartbeat"}>
              <div class="form-field">
                <label for="mf-grace">Grace period (seconds)</label>
                <input
                  id="mf-grace"
                  type="number"
                  min={1}
                  value={graceSeconds()}
                  onInput={(e) => setGraceSeconds(Math.max(1, Number(e.currentTarget.value) || 1))}
                />
              </div>
            </Show>
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

          <Show when={type() !== "heartbeat"}>
            <section class="form-section">
              <h3 class="form-section-title">Certificate & Domain</h3>
              <label class="form-checkbox">
                <input
                  type="checkbox"
                  checked={type() === "ssl" ? true : sslCheckEnabled()}
                  disabled={type() === "ssl" || !sslAllowed()}
                  onChange={(e) => setSslCheckEnabled(e.currentTarget.checked)}
                />
                Enable SSL certificate check
              </label>
              <Show when={type() === "ssl" || sslCheckEnabled()}>
                <div class="form-field">
                  <label for="mf-ssl-alert-days">SSL alert days (comma-separated)</label>
                  <input
                    id="mf-ssl-alert-days"
                    type="text"
                    value={sslAlertDays()}
                    onInput={(e) => setSslAlertDays(e.currentTarget.value)}
                  />
                </div>
              </Show>

              <label class="form-checkbox">
                <input
                  type="checkbox"
                  checked={domainCheckEnabled()}
                  onChange={(e) => setDomainCheckEnabled(e.currentTarget.checked)}
                />
                Enable domain expiry check
              </label>
              <Show when={domainCheckEnabled()}>
                <div class="form-field">
                  <label for="mf-domain-alert-days">Domain alert days (comma-separated)</label>
                  <input
                    id="mf-domain-alert-days"
                    type="text"
                    value={domainAlertDays()}
                    onInput={(e) => setDomainAlertDays(e.currentTarget.value)}
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
                  const row = () => notifState()[c.id] ?? defaultNotifRow(false);
                  return (
                    <div class="notif-row">
                      <label class="form-checkbox">
                        <input type="checkbox" checked={row().attached} onChange={() => toggleAttached(c.id)} />
                        {c.name}
                      </label>
                      <Show when={row().attached}>
                        <Show when={type() !== "heartbeat"}>
                          <label class="form-checkbox inline">
                            <input
                              type="checkbox"
                              checked={row().down}
                              onChange={() => toggleTrigger(c.id, "down")}
                            />
                            down
                          </label>
                        </Show>
                        <Show when={type() === "heartbeat"}>
                          <label class="form-checkbox inline">
                            <input
                              type="checkbox"
                              checked={row().heartbeat_missed}
                              onChange={() => toggleTrigger(c.id, "heartbeat_missed")}
                            />
                            heartbeat missed
                          </label>
                        </Show>
                        <label class="form-checkbox inline">
                          <input
                            type="checkbox"
                            checked={row().recovered}
                            onChange={() => toggleTrigger(c.id, "recovered")}
                          />
                          recovered
                        </label>
                        <label class="form-checkbox inline">
                          <input
                            type="checkbox"
                            checked={row().ssl_expiring}
                            onChange={() => toggleTrigger(c.id, "ssl_expiring")}
                          />
                          ssl expiring
                        </label>
                        <label class="form-checkbox inline">
                          <input
                            type="checkbox"
                            checked={row().ssl_invalid}
                            onChange={() => toggleTrigger(c.id, "ssl_invalid")}
                          />
                          ssl invalid
                        </label>
                        <label class="form-checkbox inline">
                          <input
                            type="checkbox"
                            checked={row().domain_expiring}
                            onChange={() => toggleTrigger(c.id, "domain_expiring")}
                          />
                          domain expiring
                        </label>
                      </Show>
                    </div>
                  );
                }}
              </For>
            </section>
          </Show>

          <Show
            when={!heartbeatInfo()}
            fallback={
              <>
                <section class="form-section">
                  <h3 class="form-section-title">Ping URL</h3>
                  <div class="test-result mono">
                    {pingUrl(heartbeatInfo()!.ping_path)}
                    <button type="button" class="btn-link" onClick={handleCopyPingUrl}>
                      Copy
                    </button>
                  </div>
                  <div class="test-result mono">curl -fsS {pingUrl(heartbeatInfo()!.ping_path)}</div>
                </section>
                <div class="detail-actions">
                  <button type="button" class="btn-accent" onClick={dismiss}>
                    Done
                  </button>
                </div>
              </>
            }
          >
            <div class="detail-actions">
              <Show when={type() !== "heartbeat"}>
                <button type="button" class="btn-ghost" disabled={testing()} onClick={handleTestCheck}>
                  Test check
                </button>
              </Show>
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
          </Show>
        </div>
      </div>
    </div>
  );
};

export default MonitorForm;
