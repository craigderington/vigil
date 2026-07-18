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

Vigil is now running at <http://localhost:8080>. The container listens on
port 8080 internally regardless of the host port you publish it on.

Optionally copy `.env.example` to `.env` to override defaults (`VIGIL_BIND`,
`VIGIL_MAX_CONCURRENCY`, `VIGIL_HOST_PORT`).

### Changing the published host port

If port 8080 is already taken on your host, set `VIGIL_HOST_PORT` (defaults
to `8080`) rather than editing `docker-compose.yml`:

```bash
VIGIL_HOST_PORT=18080 docker compose up -d
```

or put `VIGIL_HOST_PORT=18080` in `.env`. The container's internal port is
always 8080 — only the host-side mapping changes.

## Data & backups

- **Data persists** across container restarts and host reboots in the named
  Docker volume `vigil-data` (SQLite database + WAL sidecars).
- **`docker compose down -v` deletes that volume.** This is the one command
  that wipes your monitors, check history, and incidents — don't run it
  unless you mean to start over.
- **Backup one-liner** (copies the live DB file out to `./vigil-backup.db`):

  ```bash
  docker run --rm -v vigil-data:/d -v "$PWD":/out debian:bookworm-slim \
    cp /d/vigil.db /out/vigil-backup.db
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

Vigil binds `0.0.0.0:8080` inside the container (i.e. it's reachable from
your LAN if you publish the port on a non-loopback host interface) and has
**no built-in authentication**. It is designed as a trusted-network,
single-operator tool — put it behind a reverse proxy, VPN, or firewall rule
if it needs to be reachable from anywhere less trusted than your home/LAN.

## Development

- Backend: Rust workspace at `crates/vigil` (binary `vigil`, subcommands
  `serve` (default) and `healthcheck`).
- Frontend: SolidJS app in `web/`, built to `web/dist` and served by the
  Rust binary (`VIGIL_WEB_DIR`, defaults to `/srv/web-dist` in the
  container).
- `docker compose build` runs both builds (Node build stage, then Rust
  build stage) inside a multi-stage `Dockerfile`; the final image is
  `debian:bookworm-slim` running as non-root uid `10001`.
