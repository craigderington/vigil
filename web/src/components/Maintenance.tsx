import { createMemo, createResource, createSignal, For, Show, type Component } from "solid-js";
import * as api from "../api";

/**
 * Maintenance screen (P4.2 §8/Task 7): list of scheduled maintenance
 * windows plus a create form. Maintenance windows are a backend-owned CRUD
 * resource (`/api/maintenance-windows*`) — this screen doesn't touch
 * `monitor.status` at all; the client-side "MAINTENANCE" pill/dot overlay
 * lives in `maintenance_ids.ts` and is driven by SSE, independently of
 * whatever this screen renders.
 */

type Scope = "all" | "tag" | "monitors";
type Mode = "oneoff" | "recurring";
type Suppress = "alerts" | "checks";

/** `<input type="datetime-local">` has no timezone info — its value is
 *  always interpreted (and displayed) in the browser's local timezone, so
 *  `new Date(local)` and `toLocaleString()` round-trip through local time
 *  with no extra conversion needed. */
function localToEpoch(local: string): number | null {
  if (!local) return null;
  const ms = new Date(local).getTime();
  return Number.isNaN(ms) ? null : Math.floor(ms / 1000);
}

function formatLocal(epoch: number | null | undefined): string {
  if (epoch == null) return "—";
  return new Date(epoch * 1000).toLocaleString();
}

/** `target_ref` comes back from the API as the raw stored column: a
 *  JSON-encoded string (e.g. `"\"prod\""` for a tag, `"[1,2,3]"` for
 *  monitor ids) — parse it back to the real string/array for display. */
function parseTargetRef(raw: string | null | undefined): any {
  if (raw == null) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return raw;
  }
}

function describeScope(w: any): string {
  if (w.scope === "tag") return `tag: ${parseTargetRef(w.target_ref) ?? "?"}`;
  if (w.scope === "monitors") {
    const ids = parseTargetRef(w.target_ref);
    return `monitors: ${Array.isArray(ids) ? ids.join(", ") : "?"}`;
  }
  return "all monitors";
}

const Maintenance: Component = () => {
  const [windows, { refetch }] = createResource(() =>
    api.listMaintenanceWindows().catch(() => [] as any[]),
  );

  const [name, setName] = createSignal("");
  const [scope, setScope] = createSignal<Scope>("all");
  const [tagValue, setTagValue] = createSignal("");
  const [monitorIdsText, setMonitorIdsText] = createSignal("");

  const [mode, setMode] = createSignal<Mode>("oneoff");
  const [startsAtLocal, setStartsAtLocal] = createSignal("");
  const [endsAtLocal, setEndsAtLocal] = createSignal("");
  const [cronExpr, setCronExpr] = createSignal("");
  const [recurStartsAtLocal, setRecurStartsAtLocal] = createSignal("");
  const [durationMinutes, setDurationMinutes] = createSignal<number>(60);

  const [suppress, setSuppress] = createSignal<Suppress>("alerts");

  const [saving, setSaving] = createSignal(false);
  const [saveNote, setSaveNote] = createSignal<string | null>(null);

  function targetRef(): string | number[] | undefined {
    if (scope() === "tag") {
      const t = tagValue().trim();
      return t !== "" ? t : undefined;
    }
    if (scope() === "monitors") {
      const ids = monitorIdsText()
        .split(",")
        .map((s) => Number(s.trim()))
        .filter((n) => Number.isFinite(n));
      return ids.length > 0 ? ids : undefined;
    }
    return undefined;
  }

  function startsAtEpoch(): number | null {
    return localToEpoch(mode() === "oneoff" ? startsAtLocal() : recurStartsAtLocal());
  }

  function endsAtEpoch(): number | null {
    if (mode() === "oneoff") return localToEpoch(endsAtLocal());
    const s = startsAtEpoch();
    if (s == null) return null;
    return s + Math.max(0, Number(durationMinutes()) || 0) * 60;
  }

  // Live "affects N" preview (re-fetched whenever scope/target/schedule
  // changes) via the backend's body-driven `/preview` endpoint — best
  // effort: a malformed in-progress form just previews as "—" rather than
  // erroring the whole screen.
  const previewKey = createMemo(() => ({
    scope: scope(),
    target_ref: targetRef() ?? null,
    recurrence: mode() === "recurring" ? cronExpr().trim() || null : null,
    starts_at: startsAtEpoch(),
    ends_at: endsAtEpoch(),
  }));
  const [preview] = createResource(previewKey, (body) =>
    api.previewMaintenanceWindow(body).catch(() => null),
  );

  function resetForm() {
    setName("");
    setScope("all");
    setTagValue("");
    setMonitorIdsText("");
    setMode("oneoff");
    setStartsAtLocal("");
    setEndsAtLocal("");
    setCronExpr("");
    setRecurStartsAtLocal("");
    setDurationMinutes(60);
    setSuppress("alerts");
  }

  async function handleCreate() {
    setSaving(true);
    setSaveNote(null);
    try {
      const dto: Record<string, unknown> = {
        name: name().trim(),
        scope: scope(),
        starts_at: startsAtEpoch() ?? 0,
        ends_at: endsAtEpoch() ?? 0,
        suppress: suppress(),
      };
      const tr = targetRef();
      if (tr !== undefined) dto.target_ref = tr;
      if (mode() === "recurring" && cronExpr().trim() !== "") dto.recurrence = cronExpr().trim();

      await api.createMaintenanceWindow(dto);
      resetForm();
      setSaveNote("Window created.");
      refetch();
    } catch (e: any) {
      setSaveNote(e?.message ?? "Failed to create window.");
    } finally {
      setSaving(false);
    }
  }

  async function handleDelete(id: number) {
    try {
      await api.deleteMaintenanceWindow(id);
      refetch();
    } catch {
      // leave the row in the list so the operator can retry
    }
  }

  // Per-row enable/disable (spec §8: "Edit / delete / enable-disable per
  // row"). `is_active` exists specifically so a window can be turned off
  // without deleting it — a full field-editor (name/scope/schedule) is OUT
  // of scope for v1; create + toggle + delete is the v1 CRUD surface for
  // this screen. Same fire-and-refetch pattern as handleDelete above.
  async function handleToggleActive(w: any) {
    try {
      await api.updateMaintenanceWindow(w.id, { is_active: !w.is_active });
      refetch();
    } catch {
      // leave the row in the list so the operator can retry
    }
  }

  return (
    <div class="settings-view maintenance-view">
      <h2 class="settings-title">Maintenance</h2>

      <section class="form-section settings-section">
        <h3 class="form-section-title">Scheduled windows</h3>
        <Show when={(windows() ?? []).length === 0}>
          <p class="settings-note">No maintenance windows yet.</p>
        </Show>
        <For each={windows() ?? []}>
          {(w) => (
            <div class={`notif-row${w.is_active ? "" : " row-disabled"}`}>
              <span>{w.name}</span>
              <span class="settings-note">{describeScope(w)}</span>
              <span class="settings-note">
                {w.recurrence ? `recurring · ${w.recurrence}` : "one-off"}
              </span>
              <span class="settings-note">suppresses {w.suppress}</span>
              <Show when={w.is_active} fallback={<span class="status-pill paused">Disabled</span>}>
                <span class="status-pill maintenance">Active</span>
              </Show>
              <button type="button" class="btn-link" onClick={() => handleToggleActive(w)}>
                {w.is_active ? "Disable" : "Enable"}
              </button>
              <button type="button" class="btn-link danger" onClick={() => handleDelete(w.id)}>
                Delete
              </button>
            </div>
          )}
        </For>
      </section>

      <section class="form-section settings-section">
        <h3 class="form-section-title">Create maintenance window</h3>

        <div class="form-field">
          <label for="mw-name">Name</label>
          <input
            id="mw-name"
            type="text"
            placeholder="Router firmware upgrade"
            value={name()}
            onInput={(e) => setName(e.currentTarget.value)}
          />
        </div>

        <div class="form-field">
          <label>Scope</label>
          <div class="chip-row" role="group" aria-label="Scope">
            <button type="button" class="chip" aria-pressed={scope() === "all"} onClick={() => setScope("all")}>
              All
            </button>
            <button type="button" class="chip" aria-pressed={scope() === "tag"} onClick={() => setScope("tag")}>
              Tag
            </button>
            <button
              type="button"
              class="chip"
              aria-pressed={scope() === "monitors"}
              onClick={() => setScope("monitors")}
            >
              Monitors
            </button>
          </div>
        </div>

        <Show when={scope() === "tag"}>
          <div class="form-field">
            <label for="mw-tag">Tag</label>
            <input
              id="mw-tag"
              type="text"
              placeholder="prod"
              value={tagValue()}
              onInput={(e) => setTagValue(e.currentTarget.value)}
            />
          </div>
        </Show>

        <Show when={scope() === "monitors"}>
          <div class="form-field">
            <label for="mw-monitors">Monitor IDs (comma-separated)</label>
            <input
              id="mw-monitors"
              type="text"
              placeholder="1, 2, 3"
              value={monitorIdsText()}
              onInput={(e) => setMonitorIdsText(e.currentTarget.value)}
            />
          </div>
        </Show>

        <div class="form-field">
          <label>Schedule type</label>
          <div class="chip-row" role="group" aria-label="Schedule type">
            <button type="button" class="chip" aria-pressed={mode() === "oneoff"} onClick={() => setMode("oneoff")}>
              One-off
            </button>
            <button
              type="button"
              class="chip"
              aria-pressed={mode() === "recurring"}
              onClick={() => setMode("recurring")}
            >
              Recurring
            </button>
          </div>
        </div>

        <Show
          when={mode() === "oneoff"}
          fallback={
            <>
              <div class="form-field">
                <label for="mw-cron">Cron expression</label>
                <input
                  id="mw-cron"
                  type="text"
                  placeholder="0 2 * * *"
                  value={cronExpr()}
                  onInput={(e) => setCronExpr(e.currentTarget.value)}
                />
              </div>
              <div class="form-field">
                <label for="mw-recur-starts">Recurs from</label>
                <input
                  id="mw-recur-starts"
                  type="datetime-local"
                  value={recurStartsAtLocal()}
                  onInput={(e) => setRecurStartsAtLocal(e.currentTarget.value)}
                />
              </div>
              <div class="form-field">
                <label for="mw-duration">Duration (minutes)</label>
                <input
                  id="mw-duration"
                  type="number"
                  min={1}
                  value={durationMinutes()}
                  onInput={(e) => setDurationMinutes(Number(e.currentTarget.value) || 0)}
                />
              </div>
              <p class="settings-note">
                <Show when={startsAtEpoch() != null} fallback="Pick a start time and cron expression.">
                  Recurs from (local): {formatLocal(startsAtEpoch())} · each occurrence lasts{" "}
                  {durationMinutes()}m
                </Show>
              </p>
            </>
          }
        >
          <div class="form-field">
            <label for="mw-starts">Starts at</label>
            <input
              id="mw-starts"
              type="datetime-local"
              value={startsAtLocal()}
              onInput={(e) => setStartsAtLocal(e.currentTarget.value)}
            />
          </div>
          <div class="form-field">
            <label for="mw-ends">Ends at</label>
            <input
              id="mw-ends"
              type="datetime-local"
              value={endsAtLocal()}
              onInput={(e) => setEndsAtLocal(e.currentTarget.value)}
            />
          </div>
          <p class="settings-note">
            <Show when={startsAtEpoch() != null} fallback="Pick a start/end time.">
              Starts (local): {formatLocal(startsAtEpoch())} · Ends (local): {formatLocal(endsAtEpoch())}
            </Show>
          </p>
        </Show>

        <div class="form-field">
          <label>Suppress</label>
          <div class="chip-row" role="radiogroup" aria-label="Suppress">
            <label>
              <input
                type="radio"
                name="mw-suppress"
                value="alerts"
                checked={suppress() === "alerts"}
                onChange={() => setSuppress("alerts")}
              />{" "}
              Alerts
            </label>
            <label>
              <input
                type="radio"
                name="mw-suppress"
                value="checks"
                checked={suppress() === "checks"}
                onChange={() => setSuppress("checks")}
              />{" "}
              Checks
            </label>
          </div>
        </div>

        <p class="settings-note">
          Affects{" "}
          <Show when={preview()} fallback="—">
            {preview()?.affected_monitor_ids?.length ?? 0}
          </Show>{" "}
          monitor(s)
        </p>

        <div class="detail-actions">
          <button type="button" class="btn-accent" disabled={saving()} onClick={handleCreate}>
            {saving() ? "Creating…" : "Create window"}
          </button>
        </div>
        <Show when={saveNote()}>
          <div class="test-result mono">{saveNote()}</div>
        </Show>
      </section>
    </div>
  );
};

export default Maintenance;
