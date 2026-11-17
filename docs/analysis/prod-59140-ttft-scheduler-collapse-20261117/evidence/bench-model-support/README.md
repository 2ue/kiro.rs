# 模型判断新旧实现的离线基准

- `src/old.rs`：`git show origin/main:src/model/model_support.rs`（66c85a7b，修复前）
- `src/new.rs`：`git show ae0ce8ab:src/model/model_support.rs`（修复后）
- `src/main.rs`：本目录的 main.rs，先逐一比对四个候选模型的判断结果，再计时"848 个账号 × 一次模型判断 + 一次等级解析"三轮。

运行（单核、2G 上限）：

```bash
mkdir -p /tmp/bench/src && cp Cargo.toml main.rs /tmp/bench/ && mv /tmp/bench/main.rs /tmp/bench/src/
git show origin/main:src/model/model_support.rs > /tmp/bench/src/old.rs
git show ae0ce8ab:src/model/model_support.rs > /tmp/bench/src/new.rs
docker run --rm --cpus 1 --memory 2g --memory-swap 2g -v /tmp/bench:/b -w /b rust:1.92.0-alpine3.23 \
  sh -c 'apk add --no-cache musl-dev >/dev/null && cargo run --release -q'
```

2026-11-17 在本机的结果（扫描一次 848 个账号，旧实现 / 新实现）：

```
round 0: 旧=89.14ms 新=0.247ms 加速=360x 单账号 旧=105.1us 新=0.29us
round 1: 旧=88.05ms 新=0.314ms 加速=281x 单账号 旧=103.8us 新=0.37us
round 2: 旧=83.37ms 新=1.087ms 加速=77x  单账号 旧=98.3us  新=1.28us
```

新旧实现对 `claude-haiku-4.5`、`claude-opus-5`、`claude-sonnet-4-6[1m]`、`gpt-4o` 的判断结果一致（程序里的 assert）。
