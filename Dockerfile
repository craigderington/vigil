FROM node:20-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json* ./
RUN npm ci || npm install
COPY web/ ./
RUN npm run build

FROM rust:1-slim AS build
WORKDIR /src
RUN apt-get update && apt-get install -y pkg-config && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock* ./
COPY crates crates
RUN cargo build --release -p vigil

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/* \
 && useradd -r -u 10001 -m -d /home/app app && mkdir -p /data && chown app:app /data
COPY --from=build /src/target/release/vigil /usr/local/bin/vigil
COPY --from=web /web/dist /srv/web-dist
USER app
ENV VIGIL_BIND=0.0.0.0:8090 VIGIL_DB=/data/vigil.db VIGIL_WEB_DIR=/srv/web-dist
EXPOSE 8090
VOLUME ["/data"]
HEALTHCHECK --interval=30s --timeout=3s --retries=3 CMD ["/usr/local/bin/vigil","healthcheck"]
ENTRYPOINT ["/usr/local/bin/vigil"]
