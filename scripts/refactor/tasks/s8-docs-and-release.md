# 任务 s8：文档清理、README 重写与发布准备（阶段 8）

## 目标

落实设计 `docs/claude-pool-refactor/09-实施阶段与验收.md` 阶段 8 与 11 章红线第 6 条：仓库内除 `src/migration/` 的旧表名常量与 `docs/claude-pool-refactor/` 的设计文档（它们必须提到 Kiro 才能说清楚“删了什么”）外，`grep -ri kiro` 为 0。README 按新系统重写。Docker 与 CI 改名。

## 先读

`docs/claude-pool-refactor/README.md` 与全部章节（新 README 以它们为事实来源）；现有 `README.md`（758 行，结构参考）；`Dockerfile`、`docker-compose*.yml`、`.github/workflows/*.yaml`。

## 允许改动的文件

`README.md`、`docs/**`、`feature/**`、`Dockerfile`、`docker-compose*.yml`、`.github/**`、`CLOUDFLARE_TIMEOUT_FIX.md`、`STREAMING_KEEPALIVE.md`、`LICENSE`（不改）、`package.json`（根）、`tools/**`、`config.example.json`、`credentials.example.*.json`（删）、`kiro-credentials-*.json`（删）。

## A. 文档处理（C12：旧方案文档删除或改写成最终结论，不留“更正说明”）

| 路径 | 处理 |
|---|---|
| `docs/refactor-plan/`、`docs/plantree/`、`docs/kiro-*.md`、`docs/kiro-rs-root-cause-package-*/`、`docs/analysis/`（全部是 Kiro 时期的分析）、`docs/archive/`、`docs/model-routing-semantics/`、`docs/runtime-scheduler-degraded-recommended-config-*.md`、`docs/scheduler-credential-warmup-slow-start-*.md`、`docs/code-execution-isolation.md`、`docs/keepalive-implementation-summary.md`、`docs/errors.md`、`docs/testing/`、`docs/feature/` | **删除整目录/文件** |
| `docs/ai-docker-compose-deployment.md`、`docs/frontend-dev-environment.md` | 改写：去 Kiro、按新二进制名/容器名/环境变量 |
| `docs/claude-pool-refactor/` | 保留；在 `README.md` 开头加一段“实施状态：已完成，本目录为设计与实施记录”；各章中“现有代码位置 `src/kiro/...`”的引用保持（它们是历史事实） |
| `feature/` 整目录 | **删除**（全部是 Kiro 时期的 issue、evidence、audit；设计 00 章已说明 ccgate 与旧证据不再作为依据） |
| `CLOUDFLARE_TIMEOUT_FIX.md`、`STREAMING_KEEPALIVE.md` | 若内容是通用的流保活/超时说明则去 Kiro 后搬到 `docs/`；若是 Kiro 专有则删 |
| `credentials.example.*.json`、`kiro-credentials-*.json`、`credentials.json`、`config.json`（若在仓库里）、`三家去重题库.txt`、`tools/kiro-capture-sidecar/`、`tools/event-viewer.html`（Kiro EventStream 查看器） | 删除 |
| `.gitignore` | 删 Kiro 相关忽略项（`/credentials.*`、`/kiro-*`、`/kiro_*`） |

## B. README 重写（中文）

结构：

1. 项目定位（两句话：Claude Code 协议账号池调度网关；账号 baseUrl+key 接入；路由决定 usage 整形等策略）
2. 功能特性（账号池调度、路由、usage 整形六种策略、模型限制与映射、密钥与额度、控制台、多实例 Redis 协调、配置导出导入、从 kiro-rs 迁移）
3. 快速开始：编译（Rust 1.92.0、Node 22.23.0、pnpm 11.11.0 不变）、最小 `config.json`、启动、验证（`curl /healthz`、一次 `/v1/messages`）、Docker
4. 核心概念：账号、路由、策略、路由功能、下游密钥（各 3 到 5 行，链接到 `docs/claude-pool-refactor/03`）
5. 配置：`config.json` 字段表（按阶段 2 到 5 后的 `Config` 实际字段生成，从 `config.example.json` 读）、环境变量（`CLAUDE_POOL_*`）、路由定义示例
6. API 端点：`/{route}/v1/messages`、`/count_tokens`、`/models`、`/api/hello`；内置路由表；错误出口表（11 章 3.10）
7. 管理 API 与控制台（链接 08 章）
8. 从 kiro-rs 迁移（`export-legacy` / `import-legacy` 用法与注意事项，链接 07 章 4 节）
9. 测试（单测命令、集成测试环境变量、golden 测试说明）
10. 项目结构（按 11 章第 2 节目标目录，以实际 `src/` 为准）
11. 技术栈、License

不写免责声明里的 Kiro/AWS 内容；保留 MIT License 与致谢（若致谢里只有 Kiro 相关项目则删）。

## C. Docker 与 CI

- `Dockerfile`：二进制 `claude-pool`、压测二进制 `pool_loadtest`、版本归一化 awk 里的包名、`CMD`。
- `docker-compose.yml`/`deploy`/`database`/`local-infra`：镜像名 `claude-pool`、容器名 `claude-pool-*`、PG 库/用户 `claude_pool`、环境变量 `CLAUDE_POOL_*`、volume 名。
- `.github/workflows/build.yaml`、`docker-build.yaml`：二进制名、artifact 名、镜像名、测试环境变量名（`CLAUDE_POOL_TEST_POSTGRES_URL` 等）、clippy 基线脚本路径不变。
- 根 `package.json`（若有 Kiro 字样）同步。

## D. 版本与发布准备

- `Cargo.toml` 版本确认为 `1.0.0`（阶段 5 已改）。
- `cargo build --release --locked --bin claude-pool` 能产出；`./target/release/claude-pool --version` 输出 `claude-pool 1.0.0`（注意共享 target 目录在 `/tmp/kiro-rs-rust-analyzer-target`，这是本机路径不是仓库内容，不改）。
- 不打 tag、不 push。

## 自检

```
grep -rni "kiro" . --exclude-dir=.git --exclude-dir=target --exclude-dir=node_modules --exclude-dir=ui/node_modules --exclude-dir=docs/claude-pool-refactor --exclude-dir=scripts/refactor --exclude=.cargo/config.toml 2>/dev/null | grep -v "src/migration/" | head   # 应为空
cargo build -q --release --locked --bin claude-pool && /tmp/kiro-rs-rust-analyzer-target/release/claude-pool --version
cargo test --bin claude-pool -q 2>&1 | tail -5
pnpm --dir ui check && pnpm --dir ui build
test ! -d feature && test ! -d docs/refactor-plan && test ! -d docs/plantree && echo "docs cleaned"
wc -l README.md
```
