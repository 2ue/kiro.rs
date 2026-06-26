FROM node:22-alpine AS frontend-builder

RUN npm install -g pnpm

WORKDIR /app/admin-ui
COPY admin-ui/package.json admin-ui/pnpm-lock.yaml admin-ui/.npmrc admin-ui/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY admin-ui ./
RUN pnpm build

WORKDIR /app/admin-ui-daisy
COPY admin-ui-daisy/package.json admin-ui-daisy/pnpm-lock.yaml admin-ui-daisy/.npmrc admin-ui-daisy/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY admin-ui-daisy ./
RUN pnpm build

WORKDIR /app/ui
COPY ui/package.json ui/pnpm-lock.yaml ui/.npmrc ui/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY ui ./
RUN pnpm build

FROM rust:1.92-alpine AS builder

RUN apk add --no-cache musl-dev perl make

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
COPY data ./data
COPY --from=frontend-builder /app/admin-ui/dist /app/admin-ui/dist
COPY --from=frontend-builder /app/admin-ui-daisy/dist /app/admin-ui-daisy/dist
COPY --from=frontend-builder /app/ui/dist /app/ui/dist

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
