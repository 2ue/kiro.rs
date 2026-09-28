# Thinking Signature 后 Too-Long 修复计划

Status: `Implementation in progress / local fake-upstream regression passed / real upstream pending`

Last reviewed: 2026-09-28 Asia/Shanghai

## 任务边界

本任务只修复已经由生产证据和本地代码共同确认的问题：

1. Kiro 第一次返回 `THINKING_SIGNATURE_INVALID` 后，provider 会用去历史
   `reasoningContent` 的派生 body 在同一账号重试。
2. 如果这个派生 body 又被 Kiro 判定为
   `CONTENT_LENGTH_EXCEEDS_THRESHOLD`，provider 原来把 terminal reason
   包装成 `thinking_signature_retry`，handler 因而无法进入通用 too-long
   payload guard 流程。
3. payload guard retry 不能重新使用带有无效 signed thinking 的原始 body。

本任务不修改：

- 签名重试的同账号、同 region 和一次性预算边界；
- 账号选择、冷却、token refresh、外部池 fallback；
- signed thinking 的长期模型溯源设计；
- 生产机器、生产配置、生产数据库、Redis 或账号状态。

## 修复设计

### 1. 保留 provider 的职责边界

provider 仍只负责识别第一次精确的 `THINKING_SIGNATURE_INVALID` 并构造兼容性
重试。它不主动裁剪 payload，也不拥有 payload guard 策略。

provider 对签名重试后的第二次 HTTP 4xx 使用真实上游 body 分类 terminal reason。
例如 body 为 `Input is too long` 时，错误诊断保留
`reason=CONTENT_LENGTH_EXCEEDS_THRESHOLD`，不再只写
`reason=thinking_signature_retry`。

### 2. 统一 handler 的 too-long admission

handler 读取 provider attempts 的真实 terminal error 和 raw upstream fragment。
无论 too-long 出现在原始发送还是 provider 的派生发送，都进入相同的 payload
guard retry 判定。

当 attempts 表明：

- 之前发生过 `thinking_signature_retry_same_credential`；
- terminal attempt 是 too-long；

payload guard retry 以“去历史 reasoning 的 provider 派生 body”为基线，再执行
Kiro weighted guard。这样不会把已经被上游拒绝的 signed thinking 重新带回第三次
请求。

### 3. 不把签名重试等同于裁剪

实现语义是“所有 outbound Kiro body 进入公共 admission”，不是“signature retry
触发裁剪”。签名分支只提供派生 body；size/weight 计算、裁剪和 too-long 分类仍
由公共 handler/payload guard 层负责。

## 已完成

- [x] provider 保留 signature retry 后真实 4xx terminal reason。
- [x] handler 从 attempts/raw upstream fragment 识别嵌套 too-long。
- [x] payload guard retry 在 signature -> too-long 场景使用无历史 reasoning 的
  派生基线。
- [x] fake Kiro handler 矩阵：stream/non-stream 各 5 轮，复现
  `signature -> too long -> guarded retry -> success`。
- [x] 已验证第三次 body 不再包含历史 `reasoningContent`。
- [x] 原有 thinking signature provider/handler retry 矩阵保持通过。

## 待完成验证

- [ ] 在项目指定 `127.0.0.1:19023` 实例上用真实账号做低并发正常请求基线。
- [ ] 用真实上游构造可控的历史 signed-thinking 请求，记录第一次签名拒绝和
  第二次 terminal reason；不保存完整 body、token 或凭据。
- [ ] 使用修复后二进制重复同一低样本请求，确认不再把 terminal too-long
  包装成不可恢复的 `thinking_signature_retry`，并确认 guard 后请求不带坏签名。
- [ ] 运行 scoped Rust C0、相关协议/CLI gate 和 artifact inventory。
- [ ] 更新中文分析文档、implementation status 和 evidence index。

## 验收标准

1. 正常真实上游 stream/non-stream 请求成功，真实 usage 可记录。
2. fake 和真实可控签名场景中，第一次请求可出现真实
   `THINKING_SIGNATURE_INVALID`，但派生 retry 不会把同一个 signed thinking
   再发回上游。
3. signature retry 后的 too-long 会被公共 classifier 识别，并最多执行一次
   配置的 payload guard retry。
4. guard 后仍超限时 fail closed，错误显示 terminal
   `CONTENT_LENGTH_EXCEEDS_THRESHOLD`，不伪造成功、不无限重试。
5. 已开始下游流式输出后发生 read error 的 post-commit 边界不被本修复改变。
6. 不泄露 credential、token、原始 signature、完整请求正文或内部调度术语。
