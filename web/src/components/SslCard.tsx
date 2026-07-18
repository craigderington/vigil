import { createResource, createSignal, Show, type Component } from "solid-js";
import * as api from "../api";
import type { SslCert } from "../api";

export interface SslCardProps {
  monitorId: number;
  /** `store.certVersion` — bumped on each `cert_updated` SSE frame so this
   *  card's resource refetches after the backend's slow-cadence refresh or a
   *  manual `Refresh` click elsewhere. */
  certVersion?: (id: number) => number;
}

type Tier = "green" | "amber" | "red";

/** Days-remaining ring tier (§6, §11.6 #6): green > 30, amber 7–30, red < 7
 *  OR invalid OR errored. A missing cert / missing days_remaining also reads
 *  red rather than a false-green default. */
function tierOf(cert: SslCert | null | undefined): Tier {
  if (!cert) return "red";
  if (cert.error || cert.is_valid === false) return "red";
  const days = cert.days_remaining;
  if (days == null) return "red";
  if (days > 30) return "green";
  if (days >= 7) return "amber";
  return "red";
}

function formatDate(epochSeconds: number | null | undefined): string {
  if (epochSeconds == null) return "—";
  return new Date(epochSeconds * 1000).toLocaleDateString();
}

/**
 * SSL certificate card (§11.6 #6): issuer/subject/valid-until, a
 * color-graded days-remaining ring, chain/hostname/self-signed pills, a
 * Refresh button (forces an immediate handshake outside the 12h cadence),
 * and an inline error state when the last check failed.
 */
const SslCard: Component<SslCardProps> = (props) => {
  const [cert, { refetch }] = createResource(
    () => [props.monitorId, props.certVersion?.(props.monitorId) ?? 0] as const,
    ([id]) => api.getSsl(id).catch(() => null),
  );

  const [refreshing, setRefreshing] = createSignal(false);

  async function handleRefresh() {
    setRefreshing(true);
    try {
      await api.refreshSsl(props.monitorId);
      await refetch();
    } finally {
      setRefreshing(false);
    }
  }

  return (
    <section class="detail-section ssl-card">
      <div class="detail-section-head">
        <h3 class="detail-section-h">Certificate</h3>
        <button type="button" class="btn-link" disabled={refreshing()} onClick={handleRefresh}>
          Refresh
        </button>
      </div>

      <Show when={cert() != null} fallback={<div class="ssl-empty">No certificate data yet</div>}>
        <div class="ssl-card-body">
          <div class={`ssl-ring tier-${tierOf(cert())}`} data-tier={tierOf(cert())}>
            <span class="ssl-ring-days mono">
              <Show when={cert()?.days_remaining != null} fallback="—">
                {cert()?.days_remaining}
              </Show>
            </span>
            <span class="ssl-ring-unit">days</span>
          </div>

          <div class="ssl-card-details">
            <div class="ssl-detail-row">
              <span class="ssl-detail-label">Issuer</span>
              <span class="ssl-detail-value">{cert()?.issuer ?? "—"}</span>
            </div>
            <div class="ssl-detail-row">
              <span class="ssl-detail-label">Subject</span>
              <span class="ssl-detail-value">{cert()?.subject ?? "—"}</span>
            </div>
            <div class="ssl-detail-row">
              <span class="ssl-detail-label">Valid until</span>
              <span class="ssl-detail-value mono">{formatDate(cert()?.valid_until)}</span>
            </div>

            <div class="ssl-pills">
              <span class={`ssl-pill ${cert()?.chain_ok ? "ok" : "bad"}`}>
                Chain {cert()?.chain_ok ? "OK" : "broken"}
              </span>
              <span class={`ssl-pill ${cert()?.hostname_match ? "ok" : "bad"}`}>
                Hostname {cert()?.hostname_match ? "match" : "mismatch"}
              </span>
              <Show when={cert()?.self_signed}>
                <span class="ssl-pill bad">Self-signed</span>
              </Show>
            </div>

            <Show when={cert()?.error}>
              <div class="ssl-error">{cert()?.error}</div>
            </Show>
          </div>
        </div>
      </Show>
    </section>
  );
};

export default SslCard;
