# Vigil

Self-hosted uptime and certificate monitor for a single operator. Replaces
UptimeRobot-style services with a local-only app: HTTP/port/DNS/heartbeat
checks, SSL/domain-expiry tracking, and email + desktop alerts, packaged as
one Docker image with a SQLite database in a named volume.

## Quickstart (Docker Compose)

```bash
cp secrets/smtp_password.example secrets/smtp_password
# edit secrets/smtp_password with your real SMTP password (single line)
chmod 0644 secrets/smtp_password

docker compose up -d
```

Vigil is now running at <http://localhost:8099>. The container listens on
port 8090 internally regardless of the host port you publish it on.

Optionally copy `.env.example` to `.env` to override defaults. `docker
compose` forwards each of these into the container's environment:

- `VIGIL_HOST_PORT` — the published **host** port (default `8099`).
- `VIGIL_BIND` — the container-**internal** bind address (default
  `0.0.0.0:8090`). If you change its PORT, the container's Docker
  healthcheck derives its probe port from this same value, so it stays in
  sync automatically.
- `VIGIL_MAX_CONCURRENCY` — global cap on simultaneous probes in flight
  (default `25`).

### Changing the published host port

If port 8099 is already taken on your host, set `VIGIL_HOST_PORT` (defaults
to `8099`) rather than editing `docker-compose.yml`:

```bash
VIGIL_HOST_PORT=18099 docker compose up -d
```

or put `VIGIL_HOST_PORT=18099` in `.env`. The container's internal port is
always 8090 — only the host-side mapping changes.

## Data & backups

- **Data persists** across container restarts and host reboots in the named
  Docker volume `vigil-data` (SQLite database + WAL sidecars).
- **`docker compose down -v` deletes that volume.** This is the one command
  that wipes your monitors, check history, and incidents — don't run it
  unless you mean to start over.
- **In-app backup (recommended):** Settings → *Backup & restore* → **Download
  backup** exports a consistent, WAL-safe snapshot of the whole database (via
  SQLite `VACUUM INTO`) as a single `.db` file — no need to stop the container.
  It includes channel secrets (webhook/ntfy tokens, `inline:` auth) but **not**
  the SMTP password (that lives in a Docker secret). To restore, pick the file
  under *Import & replace*: Vigil validates it (rejecting non-Vigil or
  newer-schema files), writes a `pre-import-<epoch>.db` safety snapshot to
  `/data`, then atomically replaces all data in one transaction and reloads.
  Anchor-host changes from a restored backup take effect after the next restart.
- **Manual cold-copy backup** (alternative): the database runs in SQLite WAL
  mode, so a hot copy of just `vigil.db` can miss data still sitting in
  `vigil.db-wal` and yield a torn or stale backup. Stop the container first so
  it cleanly checkpoints the WAL into the main file on shutdown, copy, then
  start it back up:

  ```bash
  docker compose stop vigil
  docker run --rm -v vigil-data:/d -v "$PWD":/out debian:bookworm-slim \
    cp /d/vigil.db /out/vigil-backup.db
  docker compose start vigil
  ```

## Rotating the SMTP password

Edit `secrets/smtp_password`, then redeploy so the container picks up the
new Docker secret:

```bash
docker compose up -d
```

Docker secrets are read once at container start, so a running container
will not notice an in-place edit to the secret file without a redeploy.

## Security posture

Vigil binds `0.0.0.0:8090` inside the container (i.e. it's reachable from
your LAN if you publish the port on a non-loopback host interface) and has
**no built-in authentication**. It is designed as a trusted-network,
single-operator tool — put it behind a reverse proxy, VPN, or firewall rule
if it needs to be reachable from anywhere less trusted than your home/LAN.

**HTTP monitor auth secrets:** an `auth_ref` of `inline:<value>` stores the
literal bearer token / basic-auth password in the SQLite database, and that
value **is returned as-is by `GET /api/monitors`** over the unauthenticated
LAN API described above. Prefer `env:VAR_NAME` instead — it resolves the
secret from the container's environment at probe time and is never written
to the database or returned by the API. Reserve `inline:` for throwaway/
non-sensitive values.

## Development

- Backend: Rust workspace at `crates/vigil` (binary `vigil`, subcommands
  `serve` (default) and `healthcheck`).
- Frontend: SolidJS app in `web/`, built to `web/dist` and served by the
  Rust binary (`VIGIL_WEB_DIR`, defaults to `/srv/web-dist` in the
  container).
- `docker compose build` runs both builds (Node build stage, then Rust
  build stage) inside a multi-stage `Dockerfile`; the final image is
  `debian:bookworm-slim` running as non-root uid `10001`.
