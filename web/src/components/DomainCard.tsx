import { createResource, createSignal, Show, type Component } from "solid-js";
import * as api from "../api";
import type { DomainInfo } from "../api";

export interface DomainCardProps {
  monitorId: number;
  /** `store.certVersion` — the SAME bump signal `SslCard` uses (Task 9).
   *  `refresh_domain` also emits `Event::CertUpdated{id}` on the backend, so
   *  reusing this one signal makes both cards refetch on either card's
   *  refresh or the shared slow-cadence scheduler tick. */
  certVersion?: (id: number) => number;
}

type Tier = "green" | "amber" | "red";

/** Days-remaining ring tier (§6, §11.6 #7): green > 45, amber 7–45, red < 7.
 *  A missing row / missing days_remaining also reads red rather than a
 *  false-green default. Domain thresholds differ from SSL's — default
 *  `domain_alert_days` is `[45,30,14,7]` vs SSL's `[30,14,7,3,1]`. */
function tierOf(domain: DomainInfo | null | undefined): Tier {
  if (!domain) return "red";
  const days = domain.days_remaining;
  if (days == null) return "red";
  if (days > 45) return "green";
  if (days >= 7) return "amber";
  return "red";
}

function formatDate(epochSeconds: number | null | undefined): string {
  if (epochSeconds == null) return "—";
  return new Date(epochSeconds * 1000).toLocaleDateString();
}

function formatCsv(value: string | null | undefined): string {
  if (!value) return "—";
  return value
    .split(",")
    .map((v) => v.trim())
    .filter(Boolean)
    .join(", ");
}

/**
 * Domain registration card (§6, §11.6 #7): registrar/expiry, a color-graded
 * days-remaining ring, nameservers, registry-lock status, and a Refresh
 * button (forces an immediate RDAP/WHOIS lookup outside the 24h cadence).
 *
 * `queryable === false` is a real, definitive state — some TLDs redact or
 * rate-limit WHOIS — so it renders a distinct "not queryable" note instead
 * of a false-green ring, rather than being treated as an error.
 */
const DomainCard: Component<DomainCardProps> = (props) => {
  const [domain, { refetch }] = createResource(
    () => [props.monitorId, props.certVersion?.(props.monitorId) ?? 0] as const,
    ([id]) => api.getDomain(id).catch(() => null),
  );

  const [refreshing, setRefreshing] = createSignal(false);

  async function handleRefresh() {
    setRefreshing(true);
    try {
      await api.refreshDomain(props.monitorId);
      await refetch();
    } finally {
      setRefreshing(false);
    }
  }

  return (
    <section class="detail-section domain-card">
      <div class="detail-section-head">
        <h3 class="detail-section-h">Domain</h3>
        <button type="button" class="btn-link" disabled={refreshing()} onClick={handleRefresh}>
          Refresh
        </button>
      </div>

      <Show when={domain() != null} fallback={<div class="ssl-empty">No domain data yet</div>}>
        <Show
          when={domain()?.queryable !== false}
          fallback={<div class="ssl-empty">Not queryable — registry not queryable</div>}
        >
          <div class="ssl-card-body">
            <div class={`ssl-ring tier-${tierOf(domain())}`} data-tier={tierOf(domain())}>
              <span class="ssl-ring-days mono">
                <Show when={domain()?.days_remaining != null} fallback="—">
                  {domain()?.days_remaining}
                </Show>
              </span>
              <span class="ssl-ring-unit">days</span>
            </div>

            <div class="ssl-card-details">
              <div class="ssl-detail-row">
                <span class="ssl-detail-label">Registrar</span>
                <span class="ssl-detail-value">{domain()?.registrar ?? "—"}</span>
              </div>
              <div class="ssl-detail-row">
                <span class="ssl-detail-label">Expires</span>
                <span class="ssl-detail-value mono">{formatDate(domain()?.expiry_date)}</span>
              </div>
              <div class="ssl-detail-row">
                <span class="ssl-detail-label">Nameservers</span>
                <span class="ssl-detail-value">{formatCsv(domain()?.name_servers)}</span>
              </div>
              <div class="ssl-detail-row">
                <span class="ssl-detail-label">Registry lock</span>
                <span class="ssl-detail-value">{formatCsv(domain()?.status_codes)}</span>
              </div>
            </div>
          </div>
        </Show>
      </Show>
    </section>
  );
};

export default DomainCard;
