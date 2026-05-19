FROM node:22-alpine AS admin-ui-builder

WORKDIR /app/admin-ui
COPY admin-ui/package.json admin-ui/pnpm-lock.yaml admin-ui/.npmrc admin-ui/pnpm-workspace.yaml ./
RUN npm install -g pnpm@9.15.0
RUN pnpm install --frozen-lockfile
COPY admin-ui ./
RUN pnpm build

FROM node:22-alpine AS console-builder

WORKDIR /app/frontend
COPY frontend/package.json frontend/pnpm-lock.yaml frontend/.npmrc ./
RUN npm install -g pnpm@9.15.0
RUN pnpm install --frozen-lockfile
COPY frontend ./
RUN pnpm build

FROM rust:1.92-alpine AS builder

RUN apk add --no-cache musl-dev perl make

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
COPY migrations ./migrations
COPY --from=admin-ui-builder /app/admin-ui/dist /app/admin-ui/dist
COPY --from=console-builder /app/frontend/dist /app/frontend/dist

ENV CARGO_PROFILE_RELEASE_LTO=false \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
RUN cargo build --release --no-default-features

FROM alpine:3.21

RUN apk add --no-cache busybox-extras ca-certificates

WORKDIR /app
COPY --from=builder /app/target/release/kiro-rs /app/kiro-rs

VOLUME ["/app/config"]

EXPOSE 8990

CMD ["./kiro-rs", "-c", "/app/config/config.json", "--credentials", "/app/config/credentials.json"]
