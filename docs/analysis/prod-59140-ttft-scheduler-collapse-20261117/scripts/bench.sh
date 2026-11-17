#!/bin/sh
# 本地隔离压测：独立 docker 网络 kirobench-net，不映射主机端口，不连接任何现有服务的存储。
# 用法：
#   bench.sh infra                       启动 PG / Redis / 假 Kiro 上游
#   bench.sh app <二进制路径>            重建数据库、导入伪造账号、启动被测 kiro-rs
#   bench.sh load <标签> <并发> <秒数>   压测并采样
#   bench.sh down                        清理全部 kirobench 容器与网络
set -eu
DIR=/root/code/kiro.rs/tmp/kirobench
NET=kirobench-net
LOADTEST=$DIR/bin/kiro_loadtest
case "$1" in
infra)
  docker network inspect $NET >/dev/null 2>&1 || docker network create $NET >/dev/null
  docker run -d --name kirobench-pg --network $NET --cpus 1 --memory 1g \
    -e POSTGRES_DB=kirobench -e POSTGRES_USER=kirobench -e POSTGRES_PASSWORD=kirobench \
    postgres:18-alpine >/dev/null
  docker run -d --name kirobench-redis --network $NET --cpus 0.5 --memory 512m redis:8-alpine >/dev/null
  # 假上游：每个流先等 800ms 再分 20 段、每段间隔 100ms 输出，约 3 秒一个流
  docker run -d --name kirobench-fake --network $NET --cpus 1 --memory 512m -v $DIR/bin:/bin-bench:ro \
    alpine:3.23 /bin-bench/kiro_loadtest --fake-only true --fake-listen 0.0.0.0:19080 \
    --fake-kiro-eventstream true --fake-delay-ms 800 --fake-stream-chunks 20 \
    --fake-stream-chunk-delay-ms 100 --scenario normal-stream >/dev/null
  sleep 5
  docker exec kirobench-pg pg_isready -U kirobench
  ;;
app)
  BIN=$2
  docker rm -f kirobench-app >/dev/null 2>&1 || true
  docker exec kirobench-pg psql -q -U kirobench -d postgres -c "DROP DATABASE IF EXISTS kirobench WITH (FORCE)" -c "CREATE DATABASE kirobench"
  docker exec kirobench-redis redis-cli FLUSHALL >/dev/null
  mkdir -p $DIR/run && cp "$BIN" $DIR/run/kiro-rs && cp $DIR/config.json $DIR/run/config.json
  start_app() {
    docker run -d --name kirobench-app --network $NET --cpus 2 --memory 3g \
      -e KIRO_RS_POSTGRES_MIGRATE_ON_START=true -e RUST_LOG=info \
      -v $DIR/run:/app -w /app alpine:3.23 ./kiro-rs -c /app/config.json >/dev/null
    for i in $(seq 1 90); do
      if docker exec kirobench-app wget -q -O /dev/null http://127.0.0.1:8990/healthz 2>/dev/null; then return 0; fi
      sleep 2
    done
    docker logs --tail 40 kirobench-app; return 1
  }
  start_app
  docker stop -t 20 kirobench-app >/dev/null && docker rm kirobench-app >/dev/null
  docker exec -i kirobench-pg psql -q -U kirobench -d kirobench < $DIR/seed-credentials.sql
  start_app
  sleep 10
  docker exec kirobench-app wget -q -O - --header "x-api-key: sk-admin-kirobench-local" \
    http://127.0.0.1:8990/api/admin/credentials/summary
  echo
  ;;
load)
  LABEL=$2; CONC=$3; SECS=$4
  mkdir -p $DIR/out
  # 压测期间每 2 秒采样一次运行时延迟与容器 CPU
  ( for i in $(seq 1 $((SECS / 2 + 5))); do
      lag=$(docker exec kirobench-app wget -q -O - http://127.0.0.1:8990/readyz 2>/dev/null | sed -n 's/.*"runtimeLagMs":\([0-9]*\).*/\1/p')
      cpu=$(docker stats --no-stream --format '{{.CPUPerc}}' kirobench-app)
      echo "$(date +%s) lag_ms=${lag:-NA} cpu=$cpu"
      sleep 2
    done ) > $DIR/out/$LABEL-samples.txt 2>&1 &
  SAMPLER=$!
  docker run --rm --name kirobench-client --network $NET --cpus 1 --memory 1g \
    -v $DIR/bin:/bin-bench:ro -v $DIR/out:/out alpine:3.23 /bin-bench/kiro_loadtest \
    --base-url http://kirobench-app:8990 --route /v1/messages --model claude-opus-5 \
    --concurrency $CONC --requests 1000000 --duration-secs $SECS --scenario normal-stream \
    --timeout-secs 180 --auth-key sk-kirobench-local-0001 --report /out/$LABEL.json >/dev/null
  wait $SAMPLER || true
  docker exec kirobench-pg psql -U kirobench -d kirobench -t -A -F ' ' -c "
    select count(*) n, count(*) filter (where status='success') ok,
      round(percentile_cont(.5) within group (order by (data->'latencyTrace'->>'credentialDispatchElapsedMs')::numeric)) disp_p50,
      round(percentile_cont(.95) within group (order by (data->'latencyTrace'->>'credentialDispatchElapsedMs')::numeric)) disp_p95,
      round(percentile_cont(.5) within group (order by (data->>'firstTokenLatencyMs')::numeric)) ttft_p50,
      round(percentile_cont(.95) within group (order by (data->>'firstTokenLatencyMs')::numeric)) ttft_p95
    from usage_records where deleted_at is null" | sed 's/^/usage_records: n ok disp_p50 disp_p95 ttft_p50 ttft_p95 = /'
  ;;
down)
  docker rm -f kirobench-app kirobench-client kirobench-fake kirobench-pg kirobench-redis >/dev/null 2>&1 || true
  docker network rm $NET >/dev/null 2>&1 || true
  ;;
esac
