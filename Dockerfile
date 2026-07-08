FROM node:22-alpine AS frontend-builder

RUN npm install -g pnpm

WORKDIR /app/ui
COPY ui/package.json ui/pnpm-lock.yaml ui/.npmrc ui/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY ui ./
RUN pnpm build

FROM rust:1.92-alpine AS builder

RUN apk add --no-cache musl-dev perl make

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./

ENV CARGO_PROFILE_RELEASE_LTO=false \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
    CARGO_HTTP_TIMEOUT=600 \
    CARGO_HTTP_LOW_SPEED_LIMIT=1 \
    CARGO_HTTP_MULTIPLEXING=false \
    CARGO_NET_RETRY=10 \
    CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse
RUN mkdir -p src && printf 'fn main() {}\n' > src/main.rs && cargo fetch --locked && rm -rf src

COPY src ./src
COPY data ./data
COPY --from=frontend-builder /app/ui/dist /app/ui/dist
RUN cargo build --release --locked --no-default-features

FROM alpine:3.21

RUN apk add --no-cache busybox-extras ca-certificates

WORKDIR /app
COPY --from=builder /app/target/release/kiro-rs /app/kiro-rs

VOLUME ["/app/config", "/app/logs"]

EXPOSE 8990

CMD ["./kiro-rs", "-c", "/app/config/config.json", "--credentials", "/app/config/credentials.json"]
