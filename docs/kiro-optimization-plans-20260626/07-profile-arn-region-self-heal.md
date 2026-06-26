# profileArn 与 region 自愈实施方案

## 适用范围

本方案处理 Kiro 请求中 `profileArn`、region、agent mode、builder/social profile 的解析、缓存、自愈、诊断和管理端展示。

## 来源项目与学习点

- `Kiro-Go/proxy/kiro_api.go`：profileArn 和 region 处理更直观。
- `Kiro-Go/pool/account.go`：账号模型支持和状态字段适合管理端展示。
- 当前项目 `src/kiro/protocol.rs`：已有 `resolve_profile_arn`、`resolve_streaming_profile_arn`、`resolve_agent_mode`，应在此基础上增强，不应重写。

## 当前项目现状

当前项目已经有：

- body-level `profileArn` 注入。
- endpoint header 处理。
- social/builder/external-idp 规则。
- 部分 profileArn self-heal 日志。

当前不足：

- profileArn 来源和最后验证状态不够可见。
- region 不匹配时排查成本高。
- 自愈日志可能重复。
- 管理端无法清晰说明账号当前使用哪个 profileArn。

## 目标

- 为每个账号记录 profileArn 来源、region、最后验证时间。
- 上游返回可识别 profileArn/region 错误时，做受控自愈。
- 自愈不得影响当前请求的安全性。
- 管理端展示必须说人话，不使用内部晦涩词。

## 非目标

- 不绕过 Kiro 授权。
- 不自动猜测不可验证的 profileArn。
- 不在每次请求都调用额外上游接口。
- 不改变现有 agent mode 规则。

## 涉及文件

- `src/kiro/protocol.rs`
- `src/kiro/provider.rs`
- `src/kiro/endpoint/ide.rs`
- `src/model/config.rs`
- 账号存储模型对应文件
- 管理端账号详情页面

## 新增数据结构

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProfileArnSource {
    Configured,
    AccountCache,
    UpstreamResponse,
    TokenPayload,
    Derived,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountProfileState {
    pub profile_arn: Option<String>,
    pub profile_arn_source: ProfileArnSource,
    pub region: Option<String>,
    pub last_verified_at: Option<String>,
    pub last_error_code: Option<String>,
    pub last_error_at: Option<String>,
}
```

敏感性：

- `profile_arn` 可以在管理端展示，但不得对下游返回。
- token payload 不得存储原文。

## profileArn 解析顺序

必须按以下顺序解析：

1. 当前请求明确配置且校验通过的 profileArn。
2. 账号缓存中最近验证通过的 profileArn。
3. token payload 中可解析且 region 匹配的 profileArn。
4. 现有协议逻辑推导出的 profileArn。
5. 解析失败则按现有错误路径处理。

如果多个来源冲突：

- 已配置值优先。
- 缓存值必须有 `last_verified_at`。
- token payload 只作为补充，不覆盖明确配置。

## 自愈规则

允许自愈：

- 上游返回明确的 profileArn 缺失、region mismatch、builder/social profile 相关错误。
- 响应中提供可验证的新 profileArn。
- 新 profileArn 的 account id 或 region 与当前账号匹配。

不得自愈：

- 上游返回认证失败。
- 上游返回 quota/rate limit。
- 新 profileArn 无法解析 region。
- 新 profileArn 与当前账号明显不匹配。

自愈写回：

- 必须异步写回。
- 必须记录 `old_source`、`new_source`、`reason`、`request_id`。
- 同一账号同一错误 10 分钟内最多记录一次 warning。

## 配置与兼容策略

新增配置：

```rust
pub profile_arn_self_heal_enabled: bool, // 默认 true
pub profile_arn_self_heal_write_back_enabled: bool, // 默认 false
pub profile_arn_warning_suppress_secs: u64, // 默认 600
```

默认策略：

- 可以在内存中使用自愈结果完成当前进程后续请求。
- 默认不持久化写回，避免误写配置。
- 管理员确认稳定后再开启写回。

## 实施步骤

1. 在 `protocol.rs` 返回 profileArn 时附带 `ProfileArnSource`。
2. 在 provider 调用链记录 `AccountProfileState`。
3. 解析上游 profileArn/region 错误，归类为内部 reason。
4. 实现自愈候选验证。
5. 默认只更新内存态。
6. 增加可选持久化写回。
7. 管理端显示 profileArn 来源和最后验证时间。

## 测试方案

新增测试：

- `profile_arn_configured_source_wins`
- `profile_arn_cached_verified_source_wins_over_token_payload`
- `profile_arn_region_mismatch_is_classified`
- `profile_arn_self_heal_does_not_run_on_auth_error`
- `profile_arn_self_heal_candidate_must_match_region`
- `profile_arn_warning_is_suppressed_within_window`
- `profile_arn_state_is_not_returned_downstream`

真实测试：

- 正常账号请求。
- region 错误账号请求。
- social/builder profile 差异请求。
- 开启和关闭写回分别测试。

## 验收标准

- 管理端能看到 profileArn 来源。
- region 错误能明确定位。
- 自愈不会覆盖管理员明确配置。
- 下游感知不到 profileArn 内部细节。
- 自愈日志不会刷屏。

## 风险与回滚

风险：

- 错误写回导致账号持续失败。

规避：

- 默认不持久化写回。
- 写回前校验 region 和账号匹配。
- 记录旧值，支持恢复。

回滚：

- 关闭 `profile_arn_self_heal_enabled`。
- 如果只关闭写回，内存自愈仍可用于临时恢复。

## 不得做的事项

- 不得把 profileArn 错误直接返回给下游。
- 不得在认证失败时自愈。
- 不得每次请求都远程查询 profileArn。
- 不得覆盖管理员明确配置的值。

## 后续可选扩展

可以增加“验证 profileArn”管理端按钮，但必须只影响单个账号，不得批量触发现网请求。

