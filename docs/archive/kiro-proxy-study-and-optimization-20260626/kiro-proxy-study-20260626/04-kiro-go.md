# 项目分析：`Kiro-Go`

路径：`/Users/yuanfeijie/Desktop/procode/Kiro-Go`  
最新本地提交：`feb3437`，2026-06-18  
相关度：高

`Kiro-Go` 是一个轻量但实用的 Kiro 代理。它最值得学习的是 profileArn/region 处理、账号 quota/overage 判断、账号模型支持缓存，以及 OpenAI Responses API 适配。调度本身不如当前项目强。

## 关键文件

| 文件 | 作用 |
| --- | --- |
| `proxy/kiro_api.go` | Kiro REST/API 调用、profileArn 解析、region regionalize |
| `proxy/kiro_headers.go` | Kiro headers |
| `proxy/account_failover.go` | 多账号失败切换 |
| `pool/account.go` | 加权轮询、冷却、模型列表、quota/overage |
| `proxy/cache_tracker.go` | prompt cache 模拟 |
| `proxy/translator.go` | Claude/OpenAI 到 Kiro 转换 |
| `proxy/responses_handler.go`、`responses_*` | OpenAI Responses API |
| `proxy/openai_tool_format_test.go`、`openai_toolresult_dedup_test.go` | tool 兼容测试 |

## profileArn 和 region

`proxy/kiro_api.go` 的核心点：

- `regionFromProfileArn` 从 ARN 中提取 region。
- `kiroRegionForProfile` 优先使用 profileArn 的 region，其次 account.Region，最后 `us-east-1`。
- `regionalizeURL` 把 `q.us-east-1.amazonaws.com` 或 `codewhisperer.us-east-1.amazonaws.com` 改成 profile region 对应的 `q.{region}.amazonaws.com`。
- `ResolveProfileArn` 缺失时先 `ListAvailableProfiles`，失败后 fallback 到 refresh token 返回的 profileArn。
- Builder ID 对 ListAvailableProfiles 不支持时，会 suppress 24 小时，避免反复请求失败接口。

这部分非常值得当前项目学习。

当前项目 `src/kiro/protocol.rs` 和 `src/kiro/endpoint/ide.rs` 已经有：

- body-level streaming profileArn。
- header/query profileArn 跳过 placeholder/fallback ARN。
- External IdP fallback profileArn。
- Social profileArn。
- `credentials.effective_api_region(config)`。

但需要检查：

- 是否始终优先使用真实 profileArn 里的 region。
- 企业/IDC 账号拿到真实 profileArn 后是否持久化。
- Builder ID 不支持 profile lookup 时是否有 suppress，避免管理端/后台反复打。
- usage/models/subscription API 是否和 streaming API 用一致的 region 规则。

建议当前项目增加 `profileArn region self-check`：

- 账号保存 profileArn 后解析 region。
- 如果 `api_region` 和 profileArn region 不一致，管理端提示或自动使用 profileArn region。
- call trace 记录最终 upstream region 来源：`profile_arn` / `account_region` / `config_default`。

## 账号池

`pool/account.go` 的账号池是加权轮询：

- `Weight <= 1` 作为 1 份。
- `Weight >= 2` 复制多份到 weighted slice。
- `currentIndex` 原子递增。
- 跳过 cooldown。
- 跳过即将过期 token。
- 跳过 quota blocked，除非 overage enabled 或全局 allow over usage。
- 缓存账号支持的模型列表。

当前项目调度比它强很多，因为有 Redis lease、global concurrency、dispatch queue、RPM、session sticky。但可以学习两个点：

1. 账号支持模型列表缓存  
   当前项目已有 model compatibility，但可以在管理端更明确展示“该账号最近一次实际拉到的模型列表”和更新时间。

2. Overage 状态进入调度  
   当前项目已有账号类型/free/pro/power展示需求。后续调度可以对 subscription/overage 能力做内部权重，但要避免过度依赖 upstream usage API。

## Prompt cache tracker

`proxy/cache_tracker.go` 和当前项目 `src/anthropic/prompt_cache.rs` 思路类似：

- flatten tools/system/messages。
- canonicalize 后累积 hash。
- explicit `cache_control` 作为 breakpoint。
- explicit breakpoint 后，每个 message end 可作为隐式 breakpoint。
- min cacheable tokens：普通 1024，opus 4096。
- max cache ratio 85%。

当前项目实现更完整，有 high-cache synthesize 和 target ratio。可学习点：

- `MAX_ENTRIES_PER_ACCOUNT = 200` 这种硬上限能防止长期内存增长。
- 当前项目也应确认 prompt cache tracker 是否有 per-scope 上限和全局上限。

如果当前项目没有足够上限，后续应补：

- 每账号/每模型/每 session 上限。
- prune 触发条件。
- 管理端显示当前 cache entry 数。

## Responses API

`Kiro-Go` 有 `responses_handler.go`、`responses_history.go`、`responses_store.go` 等，对 OpenAI Responses API 做兼容。

当前项目目前主要是 Anthropic Messages 兼容。后续如果要支持更多客户端，Responses API 可以参考它：

- response id/store。
- parallel response handling。
- input/history 转换。
- tool result 去重。

但这不是当前 Kiro 代理主线 P0。

## tool 格式测试

`openai_tool_format_test.go`、`openai_toolresult_dedup_test.go` 说明它也遇到过 tool 格式和重复 tool result 问题。

当前项目在 `payload_guard.rs` 和 `converter.rs` 已经修了很多。但建议把这些外部测试场景转换成当前项目 golden cases：

- OpenAI tool call 转 Kiro。
- 重复 tool_result 去重。
- assistant tool_calls + user tool result 顺序。
- 长会话裁剪后孤立 tool_result。

## 比当前项目强的地方

- profileArn/region 自愈逻辑集中，容易读。
- Builder ID profile lookup suppress 很实用。
- Responses API 有完整入口。
- prompt cache tracker 有 per-account entry cap 的思路。
- 模型列表缓存和 overage 状态进入账号可用性判断。

## 当前项目比它强的地方

- 当前项目调度、Redis lease、RPM、并发、usage、外部池明显更强。
- 当前项目错误归一化和 trace 更完整。
- 当前项目 thinking/tool/payload guard 更系统。
- 当前项目支持 `/dfcache/*` 和管理端配置。

## 建议吸收方式

P0：

- 对当前 profileArn/region 做完整审计和测试。
- 增加 profileArn lookup suppress / cooldown。
- call trace 记录 region 来源。

P1：

- 管理端展示账号实际模型列表和更新时间。
- prompt cache tracker 增加 entry 上限和统计。
- 提取 Kiro-Go 的 tool/result 测试样本。

P2：

- 评估 OpenAI Responses API。

不建议：

- 不要照搬它的加权 slice 轮询替代当前 lease 调度。
- 不要默认开启 overage 账号高权重，需要可控策略。

