# P11 `/v1/models` 返回 OpenAI 风格混合结构，缺少官方分页字段、`created_at` 与单模型查询端点

Status: fixed-in-439e5ce (not released)
Severity: Medium
Area: aux
Discovered: 2026-09-28 协议互转审计
Verified-against: a4227c1 (2026-09-29)

## 问题与影响

所有入口（`/v1`、`/na/v1`、`/cc/v1`、`/ha/v1`、`/dfcache/{route}/v1`）的 `GET .../models` 都返回同一种结构：

```json
{
  "object": "list",
  "data": [
    {
      "id": "claude-sonnet-4-6",
      "object": "model",
      "created": 1771286400,
      "owned_by": "anthropic",
      "display_name": "Claude Sonnet 4.6",
      "type": "chat",
      "max_tokens": 64000,
      "maxInputTokens": 200000,
      "contextWindow": 200000
    }
  ]
}
```

与官方 Anthropic Models API 相比有这些偏差：

1. 顶层没有 `has_more`、`first_id`、`last_id`，多了 OpenAI 的 `object: "list"`。
2. 每一项的 `type` 是 `"chat"`，官方固定为 `"model"`。
3. 没有 `created_at`（RFC 3339 字符串），只有 OpenAI 风格的整数秒 `created`。
4. 多出 `object`、`owned_by`、`max_tokens`，以及 camelCase 的 `maxInputTokens` / `contextWindow`（同一对象里 snake_case 和 camelCase 混用）。
5. 列表里有非官方 ID：带 `-thinking` 后缀的变体（如 `claude-opus-4-7-thinking`）、Claude Code 别名（`opus`、`opusplan`、`best`、`default`、`sonnet`、`haiku`），还有点号版本（`claude-opus-4.8`）。
6. 不在静态表里的上游同步模型，`created` 取 `Utc::now()`，每次调用都会变。
7. 不支持 `GET /v1/models/{model_id}`，请求这个路径拿到的是 axum 默认的空 body 404（见 [P17](17-error-status-and-envelope-deviations.md)）。
8. 忽略 `limit`、`before_id`、`after_id` 查询参数。列表按 `id` 字典序排列，官方是按发布时间倒序（新模型在前）。

影响：

- Anthropic 官方 SDK（`client.models.list()` / `client.models.retrieve()`）拿不到 `created_at`，`type` 字面量也对不上；`retrieve` 直接 404。SDK 用 `has_more` / `last_id` 自动翻页，字段缺失时一般退化成"只有一页"，不会死循环，但属于依赖 SDK 的宽松行为（推断，未逐个 SDK 验证）。
- 按 `created` 排序或缓存模型列表的客户端，未知模型的时间戳每次都在变，排序结果不稳定，缓存键也会失效。
- 模型选择器会把 `-thinking` 变体和别名当成独立模型展示。用户以为选了另一个模型，实际上只是 thinking 开关或别名（参见 [P19](19-model-id-silent-upgrade-mapping.md)）。
- 影响面：这个端点不参与推理，不影响 `/v1/messages` 主链路，所以定级 Medium，不定 High。

## 官方协议对照

> 方向约定：kiro.rs 基于 Kiro 对外提供 Claude Code（Anthropic）协议接口。客户端（如 Claude Code CLI）发出 Claude Code 协议请求，经转换后变为 Kiro 协议，调度到最终上游 Kiro；Kiro 的响应再转换回 Claude Code 协议返回给客户端。下文引用的"Anthropic 文档/官方"一律指 **Claude Code 协议规范**，即接口侧必须满足的行为；"Kiro 侧"指上游 Kiro 协议的要求。Anthropic 官方 API 不是本项目的上游。thinking 签名均为 Kiro 原生签名，在转换中原样透传。

已验证事实（Anthropic Models API 文档，`GET /v1/models`、`GET /v1/models/{model_id}`）：

| 项 | 官方 | 当前 |
| --- | --- | --- |
| 列表顶层 | `data`、`has_more`、`first_id`、`last_id` | `object`、`data` |
| 列表项 `type` | `"model"` | `"chat"` |
| 时间字段 | `created_at`：RFC 3339，例如 `"2025-02-19T00:00:00Z"` | `created`：Unix 秒整数 |
| 名称 | `display_name` | `display_name`（一致） |
| 分页参数 | `limit`（默认 20，范围 1..1000）、`before_id`、`after_id` | 全部忽略 |
| 排序 | 新发布的在前 | `id` 字典序 |
| 单模型 | `GET /v1/models/{model_id}` 返回同结构的单个对象；不存在时 `404 not_found_error` | 无路由，返回空 body 404 |
| ID 形态 | 官方 ID 和官方别名（如 `claude-sonnet-4-5`） | 另有 `-thinking`、Claude Code CLI 别名、点号版本 |

经验推断（未验证）：

- 官方较新的 Models API 可能增加了能力类字段（如 token 上限）。本文不把具体字段名当成事实，只要求"官方必需字段齐全、扩展字段只追加"。
- Claude Code CLI 主流程不依赖 `/v1/models` 发起推理，模型列表主要影响第三方 GUI 客户端（NextChat、Cherry Studio 等）和 SDK 用户。

## 源码链与根因

- 类型定义：`src/anthropic/types.rs:55-68`（`Model`：`object`、`created: i64`、`owned_by`、`#[serde(rename = "type")] model_type`、`max_tokens`、`maxInputTokens`、`contextWindow`），`src/anthropic/types.rs:72-75`（`ModelsResponse { object, data }`）。结构从一开始就按 OpenAI `/v1/models` 设计，只额外加了 `display_name` / `type`。
- Handler：`src/anthropic/handlers.rs:5998-6007`（`get_models`）和 `src/anthropic/handlers.rs:6124-6139`（`get_models_dfcache`）都写死 `object: "list"`，不读 query，没有分页。
- 数据来源：`src/anthropic/model_capabilities.rs:492-551`（`anthropic_models`）。上游同步模型逐个转成 `Model`，第 521 行写死 `model_type: "chat"`；再把 `static_anthropic_models()` 里能解析到上游的静态项合并进来；第 549 行按 `id` 排序。
- 静态表：`src/anthropic/model_capabilities.rs:1722-1830`，含 `opus` / `opusplan` / `best` / `default` / `sonnet` / `haiku` 别名和所有 `-thinking` 变体；第 1824 行同样写死 `"chat"`。
- 时间戳：`src/anthropic/model_capabilities.rs:1832-1838`（`model_created_at`）。不在静态表里的模型走 `unwrap_or_else(|| Utc::now().timestamp())`，所以每次调用结果都不同。
- 路由：`src/anthropic/router.rs:277`、`302`、`327`、`352`、`378` 只注册了 `/models`，没有 `/models/{model_id}`。

根因：端点按 OpenAI 结构设计，没有对照 Anthropic Models API 的 schema；时间字段又复用了"静态发布时间 + 兜底当前时间"的写法，没有稳定的来源。

依赖方核查（为了确认改结构的风险）：

- `admin-ui/src`、`ui/src` 都没有调用 `/v1/models`。管理界面的模型页走 Admin API（`SupportedModelsResponse`，`src/admin/types.rs:676`），和这个端点无关。两套 UI 里出现的 `maxInputTokens` 属于 Admin 定价/能力类型（`admin-ui/src/types/api.ts:1960`、`ui/src/types/api.ts:2105`），不是这个响应。
- 本项目作为外部池客户端时，会调用别的代理的 `/v1/models`（`src/admin/service.rs:1331-1379`）。解析函数 `extract_model_ids_from_models_response`（`src/admin/service.rs:9238-9284`）按 `data` / `models` / `modelList` / `items` 数组读取 `id` / `model` / `modelId` / `model_id`。只要保留 `data[].id`，把另一个 kiro.rs 实例当外部池用就不受影响。
- `docs/ai-docker-compose-deployment.md:412` 只用 curl 检查可用性，不解析字段。

## 复现

### 最小复现（单测）

可以放进 `src/anthropic/model_capabilities.rs` 的 `mod tests`（沿用现有 `KiroAvailableModel` / `KiroModelTokenLimits` 写法）：

```rust
#[test]
fn anthropic_models_created_is_stable_for_upstream_only_model() {
    let catalog = ModelCapabilitiesCatalog::new();
    catalog.sync_from_kiro_models(vec![KiroAvailableModel {
        model_id: "claude-sonnet-4-9-20270101".to_string(),
        model_name: Some("Claude Sonnet 4.9".to_string()),
        token_limits: Some(KiroModelTokenLimits {
            max_input_tokens: Some(200_000),
            max_output_tokens: Some(64_000),
        }),
        ..Default::default()
    }]);

    let first = catalog
        .anthropic_models()
        .into_iter()
        .find(|m| m.id == "claude-sonnet-4-9-20270101")
        .expect("synced model exposed");
    std::thread::sleep(std::time::Duration::from_millis(1_100));
    let second = catalog
        .anthropic_models()
        .into_iter()
        .find(|m| m.id == "claude-sonnet-4-9-20270101")
        .expect("synced model exposed");

    // 当前实现失败：未知模型的 created 取 Utc::now()
    assert_eq!(first.created, second.created);
    // 当前实现失败：type 应为官方的 "model"
    assert_eq!(first.model_type, "model");
}
```

可以放进 `src/anthropic/handlers/tests.rs` 的 handler 级测试（复用现有的 `websearch_handler_test_router` 和 key `b07-handler-key`）：

```rust
#[tokio::test]
async fn models_endpoint_matches_anthropic_list_shape() {
    let (router, _) = websearch_handler_test_router("http://127.0.0.1:9");
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("x-api-key", "b07-handler-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

    // 当前失败：缺少官方分页字段
    assert!(value.get("has_more").and_then(|v| v.as_bool()).is_some());
    assert!(value.get("first_id").is_some());
    assert!(value.get("last_id").is_some());
    let first = &value["data"][0];
    assert_eq!(first["type"], "model");
    let created_at = first["created_at"].as_str().expect("created_at string");
    chrono::DateTime::parse_from_rfc3339(created_at).expect("RFC3339");

    // 当前失败：单模型端点不存在
    let id = first["id"].as_str().unwrap().to_string();
    let single = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1/models/{id}"))
                .header("x-api-key", "b07-handler-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(single.status(), StatusCode::OK);
}
```

### 端到端复现

```bash
# 列表形态与时间戳漂移（连续两次比较未知模型的 created）
for i in 1 2; do
  curl -s http://127.0.0.1:19023/v1/models -H 'x-api-key: <key>' \
    | jq -c '{top: keys, first: .data[0] | {id, type, created, created_at}}'
  sleep 2
done

# 单模型端点：当前 404 且 body 为空
curl -si http://127.0.0.1:19023/v1/models/claude-sonnet-4-6 -H 'x-api-key: <key>'

# 分页参数被忽略：返回全量
curl -s 'http://127.0.0.1:19023/v1/models?limit=2' -H 'x-api-key: <key>' | jq '.data | length'
```

官方 SDK 视角：

```python
import anthropic
c = anthropic.Anthropic(base_url="http://127.0.0.1:19023", api_key="<key>")
page = c.models.list(limit=2)
print(page.data[0].created_at, page.has_more)   # created_at 为 None / 属性缺失
c.models.retrieve("claude-sonnet-4-6")           # NotFoundError（空 body）
```

## 修复方案

### 候选方案

A. 完全替换成官方结构：删掉 `object` / `created` / `owned_by` 等字段。最干净，但会破坏依赖 OpenAI 形态的第三方客户端（NextChat 一类会读 `data[].id` 和 `created`，推断）。

B. 超集兼容（推荐）：顶层和每一项都补齐官方必需字段，保留原有字段作为扩展。

C. 按入口区分形态：`/cc` 和 strict profile 输出官方形态，其余入口保持现状。会增加路由维度的分支，也和"同一端点同一合同"相冲突。

### 推荐方案

采用 B，分四步：

1. 列表项的 `type` 改为 `"model"`（`src/anthropic/model_capabilities.rs:521`、`1824`）。`"chat"` 目前没有任何消费者，核查依据见上文。
2. 新增 `created_at: String`（RFC 3339，UTC），保留 `created: i64` 作为 OpenAI 兼容扩展，两者来自同一个稳定时间源：
   - 静态表里的模型用表中的发布时间；
   - 上游同步但不在静态表里的模型，用首次同步时间（持久化到能力目录，或至少进程内首次见到的时间）。进程内的选择仍会在重启时变化，只能作为最低要求。不要再用 `Utc::now()`。
3. 顶层补 `has_more`、`first_id`、`last_id`，保留 `object: "list"`。支持 `limit`（默认 20，钳到 1..=1000）、`before_id`、`after_id`。默认排序改为 `created_at` 倒序、`id` 升序做次序。考虑到已有客户端按全量读取，可以在配置里提供"未传 `limit` 时返回全量"的兼容开关，默认按官方行为。
4. 新增 `GET {prefix}/v1/models/{model_id}`（包括 dfcache 前缀），命中时返回单个对象，未命中时返回 `404 not_found_error` 的 Anthropic 错误信封。
5. camelCase 的 `maxInputTokens` / `contextWindow` 保留（避免破坏现有读取方），同时新增 snake_case 的 `max_input_tokens`，文档中注明 camelCase 已弃用。`-thinking` 和别名条目可以加一个扩展标记（例如 `"alias_of"`），不删除条目，删除与否由 P19 统一决定。

## 测试与验收

> 测试隔离：所有端到端与真实 CLI 步骤必须遵守 [测试隔离要求](README.md#测试隔离要求所有文档的测试步骤都适用)，只使用 `127.0.0.1:19023` 指定测试实例和隔离的 `HOME`/`CLAUDE_CONFIG_DIR`，Cargo 通过 `feature/tests/run-cargo-scoped.sh` 运行，不得影响本机正在运行的 Claude Code CLI 与服务。

- 单测：`created` / `created_at` 连续两次调用结果相等；`type == "model"`；`created_at` 能按 RFC 3339 解析；静态模型的 `created_at` 与静态表一致。
- Handler：`/v1/models` 顶层有 `has_more`、`first_id`、`last_id`；`limit=2` 返回 2 条且 `has_more=true`；用 `after_id=<last_id>` 翻页后无重复、无遗漏，覆盖全量；`before_id` 反向翻页。
- 单模型：已存在的 ID 返回 200，未知 ID 返回 404，body 为 `{"type":"error","error":{"type":"not_found_error",...}}`，并且带 `request-id` 头。
- 五个入口前缀（`/v1`、`/na/v1`、`/cc/v1`、`/ha/v1`、`/dfcache/{route}/v1`）各跑一次，dfcache 未定义路由保持现有的拒绝行为。
- 兼容回归：`extract_model_ids_from_models_response` 对新结构提取的 ID 集合与旧结构一致（外部池探测不受影响）。
- 用官方 Python/TS SDK 执行 `models.list()` 自动翻页和 `models.retrieve()`，结果完整。

## 兼容性与风险

- `type` 从 `"chat"` 改为 `"model"`：仓库内没有消费者；外部若有脚本按 `"chat"` 过滤，会受影响，概率很低（推断）。
- 排序改为时间倒序后，"取第一个模型当默认值"的客户端会拿到另一个默认模型。可以在发布说明里写明，必要时用配置保留字典序。
- 默认 `limit=20` 可能让只读第一页的 OpenAI 风格客户端看不到全部模型。目前静态表加上游同步模型可能超过 20 个，建议先上线"未传 `limit` 返回全量"的兼容默认，观察一段时间后再切换。
- 持久化首次同步时间需要改动能力目录的存储结构；如果只做进程内缓存，重启后时间戳仍会变化，需要在验收里写明这一边界。
- 本改动不影响推理链路，回滚只需恢复 handler 和类型。

## 修复结果与验证（2026-09-29）

- 修复（`439e5ce`）：`/v1/models` 在保留 `object`、`created`、`owned_by` 的同时，新增或修正了官方字段：`type: "model"`、RFC3339 格式的 `created_at`、顶层 `has_more`/`first_id`/`last_id`。未知模型的 `created` 改为固定值，每次调用返回的结果保持一致。
- 单测：`models_response_is_a_superset_of_the_claude_code_models_shape`。
- 真实上游：`GET /cc/v1/models` 返回 17 个模型，全部满足新的字段要求。`GET /v1/models/{id}` 和分页参数仍未实现。
- 验证环境：真实上游验证使用 `127.0.0.1:19023` 指定测试实例和隔离的 CLI `HOME`/`CLAUDE_CONFIG_DIR`，未改动本机正在运行的 Claude Code CLI 环境。证据见 `tmp/thinking-budget-local/fix-evidence-20260929/`。
