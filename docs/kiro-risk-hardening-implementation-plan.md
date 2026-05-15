# Kiro 风险加固与 Admin 可观测性优化实施方案

## 背景

当前服务作为其他服务的上游代理，对 Kiro 多账号、流式响应、usage 兼容字段和 Admin 管理面负责。前一轮实现已经具备：

1. `conversationId -> credentialId` 粘性会话调度。
2. 流式响应 EOF 后才上报账号成功，流式 read error / idle timeout / 上游错误事件按 soft failure 处理。
3. 请求级 usage record、summary、Admin API 和 Admin UI usage 面板。
4. 本地 prompt cache 模拟，默认关闭，真实 Kiro metadata usage 优先。

本轮优化不改 shell、PATH、`cc`、Cargo 全局配置或用户环境文件，只基于当前项目代码做风险加固。

## 目标

1. 修正 usage 字段语义，避免真实 metadata 下 cache read 被下游重复理解。
2. 准确记录 sticky 命中和 sticky fallback，便于排查同会话账号切换、缓存降低和高缓存异常。
3. 让普通 SSE 和 `/cc/v1/messages` 缓冲 SSE 在客户端中途断开时写入 `client_dropped` usage record。
4. 非流式响应只有在 body 成功读取、事件解析完成且未出现上游 invalid state 后，才给账号计成功。
5. Admin UI 显示账号邮箱，usage 表中明确展示账号邮箱/账号标签、sticky/fallback、compat/billable input 等排查字段。

## 非目标

1. 不引入新的全局调度策略替换现有 `priority/balanced + sticky session`。
2. 不把 balanced 改成随机轮询或全局 weighted round-robin，以免破坏同会话固定账号。
3. 不修改 Kiro 凭据文件格式之外的环境配置。
4. 不默认开启高缓存模拟或 `force-high-cache`。
5. 不改 Admin API key 的部署方式为 cookie/session，本轮只加强可观测和 UI 显示。

## 具体实施

### 1. Usage metadata 语义修正

文件：

1. `src/anthropic/cache.rs`
2. `src/kiro/model/events/additional.rs`
3. `src/anthropic/stream.rs`
4. `src/anthropic/handlers.rs`

规则：

1. `total_input_tokens` 表示完整 prompt 输入量。
2. `input_tokens` / `compat_input_tokens` 表示 Anthropic 兼容 usage 里的 uncached input tokens。
3. `cache_read_input_tokens` 单独表示 cache read。
4. `cache_creation_input_tokens` 单独表示 cache write/create。
5. `billable_input_tokens` 使用 `input_tokens + cache_creation_input_tokens`，不把 `cache_read_input_tokens` 重复计入。

真实 Kiro metadata 下：

```text
total_input_tokens = uncached_input_tokens + cache_read_input_tokens + cache_write_input_tokens
input_tokens = uncached_input_tokens
cache_read_input_tokens = cache_read_input_tokens
cache_creation_input_tokens = cache_write_input_tokens
```

如果 Kiro metadata 中的 `total_tokens` 不稳定或不符合该语义，仍以上述字段求和作为本地 usage 语义，避免依赖不明确字段。

### 2. Sticky / fallback 可观测性

文件：

1. `src/kiro/token_manager.rs`
2. `src/kiro/provider.rs`
3. `src/anthropic/handlers.rs`
4. `src/anthropic/usage.rs`
5. `admin-ui/src/components/usage-records-panel.tsx`

规则：

1. `CallContext` 增加：
   - `sticky_bound`: 本次请求是否实际使用了已有 session binding。
   - `fallback_from_sticky`: 本次请求是否因为已有绑定不可用或被本次 retry 排除而临时选了其他账号。
2. `KiroApiResponse` 和 `KiroStreamCompletion` 暴露这两个字段。
3. `UsageRecord.stickyBound` 使用真实 `sticky_bound`。
4. `UsageRecord.fallbackFromSticky` 使用真实 `fallback_from_sticky`。
5. Admin UI usage 表展示 `sticky` 和 `fallback` 标记。

### 3. Client dropped usage record

文件：

1. `src/anthropic/handlers.rs`
2. `src/anthropic/usage.rs`

规则：

1. 普通 SSE 和 buffered SSE 创建 stream guard。
2. 正常 EOF、read error、idle timeout、上游错误事件写 usage record 后标记 guard 已完成。
3. 如果 stream 被 drop 且 guard 未完成，写入：
   - `status = client_dropped`
   - `usageSource = none`
   - `errorType = client_dropped`
   - `errorMessage = downstream client dropped before upstream stream completed`
4. `KiroStreamCompletion::Drop` 继续负责 sticky soft failure，usage guard 只负责记录可观测数据。

### 4. 非流式成功上报时机

文件：

1. `src/kiro/provider.rs`
2. `src/anthropic/handlers.rs`

规则：

1. `call_api_with_context` 只返回 response 和凭据上下文，不立即 `report_success_for_session`。
2. handler 在 body 读取、eventstream 解析和 invalid state 检查都通过后，再调用 provider 的成功上报方法。
3. `call_api` 旧接口保持兼容：内部仍可在返回 response 前上报成功，避免未使用旧路径行为突然变化。

### 5. Admin UI 邮箱与关键字段显示

文件：

1. `admin-ui/src/components/credential-card.tsx`
2. `admin-ui/src/components/usage-records-panel.tsx`
3. `admin-ui/src/types/api.ts`

规则：

1. 凭据卡片标题显示邮箱；没有邮箱时显示 masked API key；再没有则显示 `凭据 #id`。
2. 凭据卡片保留 `#id`，避免同邮箱多账号时无法定位。
3. 手动新增凭据、批量 JSON 导入和 KAM 导入都把邮箱作为可选账号标签写入后端。
4. Usage 表账号列显示：
   - `#credentialId`
   - 邮箱或 masked API key 标签
5. Usage 表增加/明确显示：
   - `sticky`
   - `fallback`
   - `compat input`
   - `billable input`
   - `cache read`
   - `cache create`

## 风险控制

1. 新字段向后兼容，序列化仍使用 camelCase。
2. sticky/fallback 字段只影响记录和 UI，不改变账号选择策略。
3. client drop 记录只在 stream 被 drop 且未完成时写入，不影响响应流协议。
4. 非流式成功上报延后后，账号成功数会更准确，但历史统计曲线可能和之前略有差异。
5. metadata usage 语义修正后，`compatInputTokens` 和 `billableInputTokens` 可能低于之前错误重复计算值，这是预期修正。

## 验收标准

1. `cargo fmt --check` 通过。
2. `cargo test` 通过。
3. `cargo check` 通过。
4. `admin-ui pnpm build` 通过。
5. `git diff --check` 通过。
6. metadata usage 测试覆盖：
   - uncached=1200
   - cache_read=180000
   - cache_write=24000
   - `input_tokens = 1200`
   - `total_input_tokens = 205200`
   - `billable_input_tokens = 25200`
7. sticky/fallback 字段能从 token manager 传到 provider，再写入 usage record。
8. stream client drop 有明确代码路径写入 `client_dropped`，且不会和 success/error 重复记录。
9. Admin UI usage 表能看到账号邮箱/标签，凭据卡片能看到邮箱和账号 ID，新增/导入账号时可保留邮箱标签。

## 后续可选优化

1. JSONL usage record 改为后台异步写入并增加文件轮转。
2. 新会话选择引入余额感知评分，但保留同会话 sticky 优先。
3. Admin UI 增加时间范围过滤、分页、top credentials/top conversations 展示。
4. Admin API CORS 可按部署配置收紧到指定 origin。
