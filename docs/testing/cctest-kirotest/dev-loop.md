# kirotest 开发模式热更新

可以使用开发模式迭代，不需要每次做 release 冻结编译。Rust 服务本身不能在进程内替换代码，因此这里的“热更新”是：监听源码、debug 增量编译、编译成功后原子替换二进制、只重启 59139。编译失败时旧的 59139 进程继续运行。

## 启动循环

```bash
docs/testing/cctest-kirotest/scripts/dev-loop.sh \
  --adopt-existing \
  --jobs 2 \
  --poll-seconds 1
```

`--adopt-existing` 只用于第一次接管当前已经运行的 59139 开发进程。脚本会校验命令行中的独立开发配置路径；端口被未知进程占用时会拒绝停止。接管后 PID 写入 `/tmp/kirotest-dev/dev-loop.pid`，后续重启只管理该 PID。

后台运行并保存日志（用独立 session，避免调用终端结束时连带杀掉监听循环）：

```bash
setsid -f sh -c 'exec python3 docs/testing/cctest-kirotest/scripts/dev_loop.py \
  --adopt-existing --jobs 2 --poll-seconds 1 \
  >>/tmp/kirotest-dev/logs/dev-loop.stdout.log 2>&1'
pgrep -af 'python3 .*dev_loop.py'
```

记录监听器 PID 到 `/tmp/kirotest-dev/dev-loop-wrapper.pid`。当前运行实例由独立 session 中的 Python 监听器管理；59139 服务 PID 单独记录在 `/tmp/kirotest-dev/dev-loop.pid`。确认自动更新是否生效时，同时检查监听器和服务 PID，不能只看服务仍健康。

停止监听循环不会自动停止 59139：

```bash
kill "$(cat /tmp/kirotest-dev/dev-loop-wrapper.pid)"
```

## 单次构建检查

只构建并原子复制二进制，不停止或启动 59139：

```bash
docs/testing/cctest-kirotest/scripts/dev-loop.sh --check --jobs 2
```

二进制放在 `~/.cache/kirotest-dev/kiro-rs-dev`，增量 Cargo 目录放在 `~/.cache/kirotest-dev/cargo-target-debug`，位于系统盘而非 `/tmp` tmpfs，避免构建产物挤占 tmpfs 内存。开发 profile 关闭调试符号并保留 debug 断言与增量编译。

## 资源门禁

脚本沿用摸底阶段的暂停/恢复规则：

| 指标 | 暂停编译 | 恢复编译 |
|---|---:|---:|
| 可用内存 | `< 4 GiB` | `>= 6 GiB` |
| 可用 swap | `< 8 GiB` | `>= 10 GiB` |
| 1 分钟 load | `>= 8` | `< 6` |
| 59138 `/healthz` | 非 200 | 恢复 200 |

编译和重启前会访问 `127.0.0.1:59138/healthz` 作为只读探针。脚本不调用 59138 Admin API，不写生产 PostgreSQL/Redis，不停止生产进程。开发实例固定使用 `0.0.0.0:59139`、`/tmp/kirotest-dev/config/config.json` 和独立的开发存储。

## 验证

每次成功重启后脚本会等待 59139 重新监听。手工检查两个实例：

```bash
curl -fsS http://127.0.0.1:59139/healthz
curl -fsS http://127.0.0.1:59138/healthz
```

公网 Caddy 链路仍使用 `https://kirotest.okmcode.com`，Caddy 配置不需要随源码热更新而 reload。
