# 模型目录与价格来源

状态：实现与本地验证完成；Claude CLI 实际模型调用未通过本地 API key 认证

## 目标

- Kiro 能力目录只表达 Kiro 当前可用模型，由 Kiro `ListAvailableModels` 独立同步。
- Claude/Anthropic 价格目录独立于 Kiro 能力目录，覆盖 Claude Code 外显模型和 Fable/Mythos 族。
- LiteLLM 作为公开价格补充源；除 Kiro 能力目录、Claude 外显目录和内置规范目录外，还发现 feed 中标记为 Anthropic chat 的新 Claude 家族模型。
- 价格优先级固定为手动价格 > LiteLLM 公开价格 > 内置规范价格；同步失败保留旧快照。
- 计费选择与缓存 shaping 共用 `PricingCatalog`，缺失价格只在同一模型族内按最近版本回退。

## 当前实现边界

PostgreSQL 分两层保存价格：`model_pricing` 只存 LiteLLM/内置基价，`model_pricing_manual_overrides` 只存手动覆盖。启动迁移会把旧表里 `source='manual'` 的记录复制到覆盖表，再从基价表删除；重复启动不会重复迁移。读取时按模型 ID 合并，手动覆盖优先。刷新基价不会改动覆盖；删除覆盖后立即恢复最新公开价或内置价。

内置规范价格承载仓库已有的 Claude API 基线；LiteLLM feed 是可手动刷新的公开价格补充源。同步除查询 Kiro 能力目录、Claude 展示目录和内置目录中的 ID 外，也会发现 feed 里 `litellm_provider=anthropic`、`mode=chat` 的 Claude 家族价格项；Bedrock 等 provider、非 chat 模型不会混入。日期别名会归并到稳定的模型版本 ID。管理页的能力同步与公开价格同步相互独立；价格页同时展示生效价，并把 Kiro 能力目录中缺少精确价格的模型列为可直接配置的入口。手动价格可独立添加，不会制造假的 Kiro 能力项。

计费和缓存 shaping 继续共用 `PricingCatalog`：先跨所有同模型 ID 别名查找手动价格，再查公开/内置价格，避免虚线别名上的公开价压过点式手动覆盖；未命中时在同族内按数值版本选择最近的低档，没有低档时选择该族最早可用档。同版本多个 ID 时优先手动来源，其次公开来源，再用稳定的规范 ID 打破平局。Fable、Mythos、Haiku、Sonnet、Opus 互不借价。公开数据和手动输入都会拒绝负数、NaN、无限值；同步请求设 20 秒超时，HTTP、JSON 或无有效条目失败时保留现有快照。

## 验收

- Kiro 能力同步不再决定价格同步覆盖范围。
- LiteLLM 返回缺少 Fable/Mythos 时，已有手动价格和内置 Claude 价格不消失。
- 同模型同时存在手动、LiteLLM 和内置值时，计费使用手动值。
- Fable/Mythos 未配置价格时 `available=false` 且费用为零；配置后只在本族内回退。
- 启动恢复会分别加载内置/持久化基价和手动覆盖；管理页同步只刷新基价，手动覆盖不会被覆盖，失败不删除旧价格。
- 同模型的虚线/点式别名之间，手动覆盖始终优先于公开和内置价格。
- 公共 feed 中新增 Anthropic chat Claude 家族模型会自动进入价格表；非 Anthropic provider、非 chat 和无效费率不会被采纳。
- HTTP 失败和无效 JSON 的回归测试证明同步保留现有价格快照。
- 手动覆盖跨公开价格刷新仍优先；删除后恢复刷新后的公开价。
- Kiro 动态能力模型会进入未配置价格入口；手动定价不创建能力记录。
- Rust 价格选择、同步候选、数据库迁移/持久化和 UI 构建通过验证。

## 本地验证

- 2026-11-15：Rust 价格目录 `24/24`；手动价格数值校验 `1/1`；schema guard `2/2`。
- 2026-11-15：隔离 PostgreSQL 16 的旧手动价迁移、价格持久化与 schema 兼容测试各 `1/1`；一次性容器 `kiro-pricing-final-validation-20261114` 已移除。
- 2026-11-15：默认与 `--no-default-features` Rust 全量测试均为 `3206 passed / 18 ignored`，负载工具各 `31/31`；两组全量结果先于最后一轮 Clippy 告警重构，重构后的价格、输入校验与 schema guard 定向测试再次通过。
- 固定 Rust `1.92.0` 的 Clippy 基线通过：`734` 条告警，仓库基线允许 `849` 条且无新增 lint/file bucket 超限；`cargo fmt --all -- --check`、`cargo build --release --locked`、构建产物 inventory 均通过。
- `ui` 的 `npm run build` 通过；存在既有的大 chunk 体积提示。
- 使用指定配置在 `127.0.0.1:19023` 启动候选服务，`/readyz` 返回 `ready`。Claude Code CLI `2.1.280` 连接服务后收到 `401 Invalid API key`，未得到模型响应；候选进程已停止，端口已释放。
- PostgreSQL 集成验证使用本轮新建的一次性容器，没有复用本机现有数据库容器；一次性容器和候选二进制临时目录已清理。
