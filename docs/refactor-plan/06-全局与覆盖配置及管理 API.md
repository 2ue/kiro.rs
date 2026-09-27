# 全局与覆盖配置及管理 API

## 1. 配置所有权

当前 `RuntimeConfig` 把几十类配置放在一个 JSON 中。重构后按 owner 拆成以下 schema，但底层仍可在一个版本化配置文档中存储，以便原子发布：

| Schema | 负责内容 | 可覆盖层级 |
| --- | --- | --- |
| `GlobalGatewayConfig` | 监听、认证、body limit、admission、代理、TLS、压缩、日志 | global |
| `DispatchConfig` | priority/balanced、并发、RPM、queue、lease、sticky、cooldown、retry budget | global → route → channel → account |
| `RoutePolicyConfig` | path/model/protocol 匹配、local/external allow/deny、fallback/rescue | global → route |
| `CacheConfig` | cache templates、path overrides、reported usage、creation control、bounds | global → route → channel → account |
| `ProtocolConfig` | 入站 profile、model mapping、payload guard、reasoning | global → route → provider/channel |
| `ProviderConfig.kiro` | Kiro endpoint、region、token、Kiro conversion、quota | provider/channel/account |
| `ProviderConfig.claudeCode` | Base URL、headers、TLS、raw/normalized、stream、usage projection | channel/account |
| `ModelConfig` | capabilities、aliases、pricing、supported model rules | global → provider/channel/account |
| `ObservabilityConfig` | usage materialization、metrics、debug capture、retention | global → provider/channel |

覆盖值使用三态：`unset`（继承）、`set`（覆盖）、`clear`（显式清空可继承数组/可选值）。不能用普通 JSON null 同时表达“清空”和“字段缺失”。

## 2. 合并和校验

合并顺序固定为：

```text
defaults → global → route → channel → account → request facts
```

每次保存执行：schema validation → normalization → semantic validation → diff/impact report → versioned commit。归一化必须保留现有行为，例如路径前缀 canonicalization、route rule 去重、cache patch 去空、finite number 检查和 min/max 边界。

计算出的 `EffectiveConfig` 必须能通过 Admin API 预览，显示每个字段来自哪一级。Provider capability 过滤发生在合并之后：如果 account provider 不支持某字段，则返回明确的 unsupported capability 错误，而不是静默丢弃。

## 3. API 版本

新 API 以 `/api/v2/admin` 和 `/api/v2/runtime` 为主，旧 `/api/admin` 在迁移期间保留适配层。建议端点：

```text
GET    /api/v2/admin/channels
POST   /api/v2/admin/channels
PATCH  /api/v2/admin/channels/{id}
DELETE /api/v2/admin/channels/{id}

GET    /api/v2/admin/accounts?channelId=&providerType=
POST   /api/v2/admin/accounts
PATCH  /api/v2/admin/accounts/{id}
POST   /api/v2/admin/accounts/{id}/validate
POST   /api/v2/admin/accounts/{id}/refresh

GET    /api/v2/admin/routes
PUT    /api/v2/admin/routes/{id}
GET    /api/v2/admin/cache/policy
PATCH  /api/v2/admin/cache/policy
POST   /api/v2/admin/cache/policy/preview
POST   /api/v2/admin/cache/policy/rollback

GET    /api/v2/admin/config/effective?routeId=&channelId=&accountId=
GET    /api/v2/admin/config/versions
POST   /api/v2/admin/config/validate

GET    /api/v2/admin/usage/records
GET    /api/v2/admin/usage/dashboard
GET    /api/v2/admin/audit-logs
```

所有 GET 返回 `ETag`/`version`；PATCH 必须带 `If-Match`，冲突返回 409 并给出 server/client diff。秘密更新使用单独 endpoint 或 write-only field，读取时只返回 fingerprint/masked value。

局部保存示例（只改缓存路径，不覆盖其他运行配置）：

```http
PATCH /api/v2/admin/cache/policy
If-Match: "cache-policy:42"
Content-Type: application/merge-patch+json

{
  "routes": {
    "/cc": {
      "strategy": "current_high_cache",
      "routeNamespace": {"$set": true},
      "simulation": {"targetReadRatio": {"$set": 0.98}}
    }
  }
}
```

服务端返回新的 version、规范化后的完整 policy、字段来源和影响范围。`$set`/`$clear` 是 API 层的三态操作，不能把 `null` 静默解释成清空。保存失败时不写入部分字段；未知字段保留在 document 中并在响应中标为 `preservedUnknownFields`。

## 4. 旧 API 兼容映射

兼容层将：

- `/credentials*` 映射到 `accounts?providerType=kiro`。
- `/external-pools*` 映射到 `channels/accounts?providerType=claude_code_upstream`。
- `/config/load-balancing` 映射到 `DispatchConfig`。
- `/config/runtime` 读取多个 v2 schema 的聚合视图；旧整体 PUT 转成多个局部 patch，并拒绝删除未知字段。
- 旧 `externalPoolRouteMode`、`localPoolRouteMode` 映射到统一 `RoutePolicyConfig.targets[].mode`。
- 旧 usage record 中的 `credentialId`、`externalPoolId` 同时填充 `accountId`、`channelId`，并保留旧字段到兼容期结束。

旧响应中的 Kiro credential 专属字段只在 `providerType=kiro` 的 account projection 中返回；外部账号响应不可出现 refresh token、authRegion、Kiro overage 等字段。

## 5. 数据表建议

保留现有 usage/audit/proxy 表的兼容读取，在新 migration 中增加：

```text
channels(id, provider_type, name, base_url, config_json, revision, enabled, ...)
accounts(id, channel_id, provider_type, label, secret_ref, limits_json,
         provider_state_json, revision, enabled, ...)
route_policies(id, match_json, targets_json, fallback_json, revision, ...)
config_documents(id, scope_type, scope_id, schema_version, document_json,
                 revision, updated_at, updated_by)
cache_policies(id, scope_type, scope_id, policy_json, revision, ...)
```

每个 provider-specific JSON 都必须带 schema version；迁移器要幂等、可观察、可回滚。旧 `credentials` 行先复制为 `channels(provider_type=kiro)` + `accounts`；旧 `external_pools` 行先复制为 `channels(provider_type=claude_code_upstream)`，原 ID 写入 `legacy_source_id`。
