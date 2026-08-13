# Upstream Error Diagnostic Privacy And Bounds

Status: `focused-provider-handler-pass / persistent-storage-frozen-load-pending`

Severity: P0/P1

## 问题、现象与影响

本地 Kiro provider 已把诊断响应体限制为 1 MiB，但多条普通 API 失败路径仍将完整 body 拼入 warning/error 日志、`KiroCredentialAttempt.error_message`、transient scheduler reason 和最终 provider error。handler 随后可能把该字符串保存为 usage `error_message/error_detail`，管理 UI、数据库和证据归档因此可看到上游返回的 prompt/tool/schema 片段、内部异常或动态 secret。

该问题不需要 `bashHash...`、`Tool results provided` 或 thinking 指纹。它通常表现为错误详情/日志异常，不是正常 assistant content；但同样违反“不把内部协议和内容带到用户可见/持久化诊断面”的要求。

## 根因

response body 同时承担“分类输入”和“诊断文本”两个所有权。代码需要读取 body 判断 risk control、quota、invalid model/image/tool schema、profile ARN 和 transient 状态，却在分类后继续复用原始字符串构造日志与 attempt/error。读取上限只控制内存，不能提供隐私边界。

HTTP 200 JSON exception 和 MCP 已开始使用 fixed classification/body byte count，但普通 API 400/401/403/408/429/5xx、2xx non-eventstream、profile/model discovery 仍有独立旧路径，不能从单个 JSON sniffer 测试外推为全部安全。

## 复现方法

fake Kiro upstream 对每个状态/Content-Type 返回唯一 marker，并让 marker 同时出现在 JSON code/message、plain text、HTML、分块 body 和接近 1 MiB 边界。stream/non-stream、1/20/60 credentials 各 5 轮，捕获：公开响应、DEBUG/INFO/WARN 日志、UsageRecord JSON、credential attempt chain、scheduler state、PostgreSQL/Redis usage snapshot。

另测 body read timeout、Content-Length 超限、chunked 超限和 malformed UTF-8；错误后正常请求恢复 5/5。实际 HTTP hits 必须受共享 inference budget 约束，隐私修复不能引入额外读取、重试或账号扫描。

## 修复方案

- body 只在局部分类器内存在；分类输出为固定 enum/reason、HTTP status、body bytes、字段存在性和必要的 retry-after。
- provider error、attempt、scheduler reason 和普通日志只保存固定低基数分类；不得保存 raw code/reason/message/body preview。
- 为 payload-too-long、context-full、invalid model/image/tool schema、profile ARN、risk/quota/auth 建立固定分类 token，handler 依据 typed metadata 或固定 token 映射公开错误，不再反向扫描原文。
- 只有明确 opt-in、受上限且经过统一 redaction 的本地诊断 artifact 才可保存正文；默认 production 日志/usage 不允许。
- profile/model discovery 的管理测试也使用相同 body limit 和固定错误，不因其是 auxiliary API 放宽。

## 当前修复与验证证据

当前源码已把 body 限定为局部分类输入：`read_upstream_body_strict` 负责 deadline、字节上限和 UTF-8，`api_failure_diagnostic` 只接收固定 class/status/body-bytes/retry-after/content-type/reason，不接收正文。普通 API、MCP、ListAvailableProfiles 和 model discovery 的 tracing、attempt 与 scheduler reason 静态复核未再发现 raw response body interpolation。

2026-07-18 当前 dirty tree 的动态矩阵已通过：status/JSON 13 类 x stream/non-stream x 1/20/60 pool x 5 轮，共 390 个 provider outcome、990 个受共享预算约束的 send；transport/body 6 类同矩阵，共 180 个 outcome、540 个 send。每个 outcome 都扫描 error text、serialized attempts、scheduler snapshot 与 DEBUG log，private marker 为 0；错误 class/status/cooldown 和真实 send 数保持可诊断。两个初始精确测试分别为 `141.73s` 与 `243.67s`。

完整树随后暴露了一个独立但相关的 deadline 缺陷：1 秒 header timeout 在 executor 压力下于 1.651 秒接受了 fake 500。公共 header/body helper 改为 deadline-first 后，transport/body 全矩阵以 `245.74s` 再次通过；deadline 后 `r11` 为 `1708/1708` 非 ignored，当前 `r12` 为 `1715 passed / 0 failed / 6 ignored`，Rust 1.92.0 `cargo check --all-targets` 零 warning。完整命令、计数和清理见 [隐私证据](../evidence/upstream-error-privacy-bounds-20260718.md) 与 [deadline 证据](../evidence/http-deadline-runtime-starvation-20260718.md)。

Router 层另有五轮 HTTP-200 JSON exception，逐轮扫描公开 body、UsageRecord 和 DEBUG log；manual provider、model/profile discovery 和 MCP body/status spoofing 也在完整树内。当前可以关闭“普通内存态 provider 错误正文仍直接进入 error/attempt/scheduler/log”的 focused 缺陷，不能关闭最终发布门禁。

仍待验收：用同一 marker 通过冻结临时 HTTP 服务；对隔离 PostgreSQL/Redis 持久化结果做实际查询或执行 fail-closed 验证程序；完成 mixed error burst/recovery、RSS/FD、C1-C4/L1-L5 和最终 binary SHA 绑定。每类 marker 在这些面也必须为 0，且固定分类、HTTP status、retry-after、body byte count 和 request/error ID仍可诊断。

## 残余风险与回滚

未来上游新增错误 code 时应先落为 `unknown_upstream_error`，不能为了快速排障恢复 raw body。过度脱敏可能降低定位精度，因此必须保留固定分类、状态、bytes、request/error ID 和 attempt 时序。回滚可以调整分类映射，但不得恢复原始 response body 进入日志、usage、scheduler reason 或公开错误。
