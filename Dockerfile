FROM node:22.23.0-alpine3.23 AS frontend-builder

ARG PNPM_VERSION=11.11.0
RUN npm install -g "pnpm@${PNPM_VERSION}" && pnpm --version

WORKDIR /app/admin-ui
COPY admin-ui/package.json admin-ui/pnpm-lock.yaml admin-ui/.npmrc admin-ui/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY admin-ui ./
RUN pnpm build

WORKDIR /app/ui
COPY ui/package.json ui/pnpm-lock.yaml ui/.npmrc ui/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY ui ./
RUN pnpm build

WORKDIR /app/console
COPY console/package.json console/pnpm-lock.yaml console/.npmrc console/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY console ./
RUN pnpm build

FROM rust:1.92.0-alpine3.23 AS rust-base

RUN apk add --no-cache musl-dev perl make

ENV CARGO_PROFILE_RELEASE_LTO=false \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
    CARGO_HTTP_TIMEOUT=600 \
    CARGO_HTTP_LOW_SPEED_LIMIT=1 \
    CARGO_HTTP_MULTIPLEXING=false \
    CARGO_NET_RETRY=10 \
    CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse

# Every release bumps the kiro-rs version in Cargo.toml/Cargo.lock. Normalizing it here keeps
# the dependency stage's inputs byte-identical across releases, so its cached layer is reused
# and only a real dependency change (Cargo.lock, features, toolchain) rebuilds dependencies.
FROM rust-base AS dependency-manifest
WORKDIR /manifest
COPY Cargo.toml Cargo.lock ./
RUN awk '!done && /^version = / { $0 = "version = \"0.0.0\""; done = 1 } { print }' \
        Cargo.toml > Cargo.toml.normalized \
    && mv Cargo.toml.normalized Cargo.toml \
    && awk 'prev == "name = \"kiro-rs\"" && /^version = / { $0 = "version = \"0.0.0\"" } { print; prev = $0 }' \
        Cargo.lock > Cargo.lock.normalized \
    && mv Cargo.lock.normalized Cargo.lock

# Compiles every dependency against a placeholder main.rs. This layer is cached between
# releases; the root crate's placeholder artifacts are removed so the real build below always
# recompiles kiro-rs from the actual sources.
FROM rust-base AS dependencies
WORKDIR /app
COPY --from=dependency-manifest /manifest/Cargo.toml /manifest/Cargo.lock ./
RUN mkdir -p src \
    && printf 'fn main() {}\n' > src/main.rs \
    && mkdir -p src/bin && printf 'fn main() {}\n' > src/bin/kiro_loadtest.rs \
    && cargo build --release --locked \
    && rm -rf src target/release/kiro-rs target/release/kiro-rs.d \
        target/release/kiro_loadtest target/release/kiro_loadtest.d \
        target/release/deps/kiro_rs-* target/release/deps/kiro_loadtest-* \
        target/release/.fingerprint/kiro-rs-* target/release/incremental

FROM dependencies AS builder
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY data ./data
COPY --from=frontend-builder /app/admin-ui/dist /app/admin-ui/dist
COPY --from=frontend-builder /app/ui/dist /app/ui/dist
COPY --from=frontend-builder /app/console/dist /app/console/dist
RUN cargo build --release --locked --bin kiro-rs \
    && ./target/release/kiro-rs --version

FROM alpine:3.23

RUN apk add --no-cache busybox-extras ca-certificates

WORKDIR /app
COPY --from=builder /app/target/release/kiro-rs /app/kiro-rs

VOLUME ["/app/config", "/app/logs"]

EXPOSE 8990

CMD ["./kiro-rs", "-c", "/app/config/config.json", "--credentials", "/app/config/credentials.json"]
