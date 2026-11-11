# kiro-manager-lite 协议操作与版本管理调研（2026-11-10）

## 结论

`kiro-manager-lite` 值得吸收的是“协议操作按职责拆分、refresh token 轮换先落盘、Profile/模型/额度查询使用统一账号上下文、IDE 与 SDK 版本按真实安装包配对”。它没有可直接移植的“自动查询 Kiro 客户端版本”实现，也没有同账号多协议端点 fallback；它的自身 updater 只更新管理器程序。

因此当前 Rust 项目应先建立官方 operation contract，再把端点/Region 调度接入已有重试管线；版本管理先做“静态真实版本配对 + 持久化 active + 显式 apply/rollback”，metadata 自动发现与灰度验证后置。

## 参考版本与源码状态

- 本地参考仓库：`/Users/yuanfeijie/Desktop/procode/kiro-manager-lite`
- 版本：`v1.0.49`
- HEAD：`f7c233af98a052a45ad043ed915e55bfecf0e4d1`
- 工作树：干净

## 一、版本协议的真实做法

### 1. 版本来源不是运行时 metadata

`src/shared/kiroVersion.ts:1-70` 从静态 `src/shared/data/kiro-sdk-versions.json` 读取 IDE 与 `@aws/codewhisperer-streaming-client` 的配对。版本表由 `scripts/kiro-sdk-versions.sh:21-163` 下载官方 Kiro 安装包、读取 `Info.plist` 和 `kiro-agent` 内的 package.json 后生成。

这条链路的价值在于：SDK 版本来自真实安装包，不靠猜测，也不会把 IDE 版本和 SDK 版本拆开。当前文件首条记录为 Kiro IDE `1.4.121` / SDK `1.0.44`，但这只是参考项目生成时的静态快照。

### 2. 手工切换立即生效

`src/renderer/src/components/tools/KiroVersionCard.vue:104-149` 保存 IDE/SDK 组合；`src/main/ipc.ts:296-309` 通过 `setKiroVersions` 更新主进程运行时值；后续请求立即使用新组合。

`src/main/kiroEndpoints.ts:221-241` 统一构造：

- `KiroIDE-<ide-version>-<machine-id>`；
- `aws-sdk-js/<sdk-version>`；
- `api/codewhispererstreaming#<sdk-version>`；
- `x-amz-user-agent` 与普通 `User-Agent` 使用同一版本快照。

这证明“客户端版本”在当前生态里首先是请求身份和准入参数，不等于服务端下载或安装 Kiro IDE。

### 3. 迁移与回退能力有限

`src/main/store.ts:550-583` 只把旧默认组合 `0.12.333/1.0.39` 迁移到新默认 `1.4.0/1.0.44`；用户主动选过的组合保留。项目没有 candidate、last-known-good、自动稳定性验证或版本回退栈。

`src/main/updater.ts:1-32,160-245` 查询的是 `kiro-manager-lite` 自身 GitHub Release，不是 Kiro 客户端版本 metadata。

## 二、账号官方操作的可借鉴结构

### 1. refresh token 是串行、持久化优先的状态变更

`src/main/proxyUpstream.ts:247-291` 明确说明 refresh token 会轮换；同一账号同时只能有一次 refresh；刷新结果先写回账号存储，再更新内存对象。`src/shared/errors.ts:1-20` 也把旧 refresh token 立即失效作为错误模型的一部分。

当前 Rust 项目应保持同一合同：

```text
读取账号快照 -> 单账号 refresh admission -> 调官方 refresh
-> 校验响应 -> 先持久化 access/refresh/expires/profile
-> 持久化成功后发布内存快照 -> 其他 operation 使用新 generation
```

刷新失败不能把新 refresh token 丢在临时变量中，也不能让并发 refresh 使用同一个旧 token。

### 2. Profile 不是所有认证类型都同一语义

`src/main/kiroAuth.ts:32` 记录了 runtime operation 对 `profileArn` 的依赖差异；`src/main/proxyUpstream.ts:629-676` 为账号类型构造 Profile 候选，并在需要时先调用 `ListAvailableProfiles`。

应把以下情况分开：

- Social：可能没有可列出的 Profile；
- Builder ID/IdC：可能需要兼容占位 ARN 或真实 Profile；
- Enterprise：优先真实 Profile ARN，并绑定其 API Region；
- API Key：不走 OAuth refresh，也不应强行带 OAuth Profile。

### 3. 额度、模型与推理是独立 operation

Lite 的 `kiroApi.ts`、`kiroChat.ts`、`proxyUpstream.ts` 将 refresh、usage、Profile、ListAvailableModels 和对话发送分开构造，但共享同一账号凭据和统一 UA builder。模型列表会保存缓存，代理首次请求模型目录时再按需刷新；失败信息和最后检查时间独立记录。

当前 Rust 设计也应使用同一 `CredentialProtocolContext`，但保留 operation 边界：

```text
refresh_token
get_usage_limits
get_user_info / get_user_usage_and_limits
list_available_profiles
list_available_models
inference
```

一个 operation 成功，不得推导其他 operation 必然成功；例如模型列表 403 不等于 refresh token 失效，Profile 列表为空也不等于账号不可用。

## 三、对当前 Rust 项目的直接落点

### 1. 先做协议契约表，不先做端点乘积

第一阶段要为每个账号记录独立结果：

```text
imported
refresh_ok
usage_ok
profile_query_ok / profile_not_supported
models_ok
inference_ok
last_error_kind
checked_at
protocol_generation
```

导入只负责字段归一化；刷新、额度、Profile、模型和推理验证分别执行，避免“导入成功”被误写成“账号可用”。

### 2. 版本状态不能继续使用单一 `LATEST`

Lite 证明了版本配对需要真实来源，但没有解决自动发现和回退。因此 Rust 应持久化：

```text
configured_version
candidate_version
active_version
active_mode: configured | pinned | auto_applied
last_known_good_version
previous_active_version
version_generation
```

`canary` 是账号/标签范围，不是 `active_mode`；它只能对命中的账号覆盖 candidate。`configured_version` 是回退基线，但应另存 `configured_verified`，不能把“有配置值”当成“已验证稳定”。

### 3. Lite 的版本配对逻辑应作为证据源，不作为自动升级器

可以移植：

- 从官方安装包提取 IDE/SDK 配对；
- 只接受规范化版本号；
- 所有 operation 共用统一 UA builder；
- 保存后立即对后续请求生效；
- 记录版本表来源、提取时间、SDK package 明细和 status。

不能直接移植：

- 没有官方 Kiro metadata 自动查询；
- 没有 Kiro IDE 下载/安装；
- 没有稳定性评分和自动回退；
- 管理器自身 GitHub updater 与 Kiro 请求版本无关。

## 四、与 FREE 账号本地验证的关系

根目录凭据来源必须固定为：

`kiro-credentials-2026-09-16T12-11-07-416Z.json`

当前摘要：147 条，全部标记 `KIRO FREE`；其中 Social 137 条、IdC 10 条、禁用 0 条。根目录 `credentials.json` 的 5 条是 `KIRO PRO`，不属于本轮验证范围。

测试只允许从上述 FREE 文件选择账号；报告只记录数量、认证类型、凭据 ID 摘要、结果类别和脱敏错误 ID，不记录 token、邮箱或完整 Profile ARN。

验证顺序固定为：

1. 先以 `autoDiscoverSupportedModels=false` 导入多条 FREE 账号，确认导入解析和重复处理；
2. 分别从 Social、IdC 选择少量非禁用记录，串行验证 refresh；
3. 对 refresh 成功的账号查询 usage；
4. 再查询 Profile 和 models；
5. 最后才做最小 inference smoke；
6. 每项操作保存独立结果，不把某一步失败归因到其他 operation。

这套顺序用于验证当前实现与参考项目的协议合同，不用于先行决定端点轮换策略。端点/Region 故障注入使用 fake upstream，避免把真实 FREE 账号当作重试实验器。

## 五、必须保留的设计边界

- endpoint、API Region、auth Region、账号身份四个维度分开；
- 同账号 endpoint fallback 复用同一 token、Profile、machine ID、代理和版本快照；
- 账号级错误直接进入账号调度，host/Region 级故障才允许同账号候选；
- 首个非空下游字节提交后禁止重放；HTTP 响应头已收到但尚未提交首字节时仍属于可恢复窗口；
- 5xx 跨候选必须通过 `send_stage`、`RequestIdentityGeneration`、幂等/请求体可重建和共享预算门禁；
- Profile ARN 与 API Region 兼容性不确定时不盲切，相关行为由独立开关控制；
- 版本发现、版本应用、稳定版本和显式回滚分开持久化；关闭自动检查不隐式降级 active。

## 依据

- `/Users/yuanfeijie/Desktop/procode/kiro-manager-lite/src/shared/kiroVersion.ts`
- `/Users/yuanfeijie/Desktop/procode/kiro-manager-lite/scripts/kiro-sdk-versions.sh`
- `/Users/yuanfeijie/Desktop/procode/kiro-manager-lite/src/main/kiroEndpoints.ts`
- `/Users/yuanfeijie/Desktop/procode/kiro-manager-lite/src/main/proxyUpstream.ts`
- `/Users/yuanfeijie/Desktop/procode/kiro-manager-lite/src/main/kiroApi.ts`
- `/Users/yuanfeijie/Desktop/procode/kiro-manager-lite/src/main/kiroChat.ts`
- `/Users/yuanfeijie/Desktop/procode/kiro-manager-lite/src/main/store.ts`
- `/Users/yuanfeijie/Desktop/procode/kiro-manager-lite/src/main/updater.ts`

