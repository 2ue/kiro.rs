# 远程图片/文档辅助请求、资源上限与 SSRF 连接绑定

Status: `focused-handler-pass / cli-load-and-release-candidate-pending`

Severity: P1

Date: 2026-07-16

## 问题、现象与影响

`safe` 图片处理默认会把 Messages 中的远程 `image`/`document` URL 下载到本进程，再转为 base64。修复前只有“每个响应最多 20 MiB”这一条局部限制，因此一个很小的下游 JSON 可以触发：

- 无上限个数的辅助 HTTP 请求及 redirect；
- 每个源各自最多 25 秒、所有源串行累加的长等待；
- 多个 20 MiB 下载、base64 膨胀、JSON 重写和上游序列化同时驻留；
- `count_tokens` 请求执行与推理请求相同的远程下载；
- 并发请求叠加后出现辅助 RPM、RSS、FD、连接和延迟放大。

另一个独立但同路径的安全问题是：旧实现先通过 `tokio::net::lookup_host` 检查 DNS 结果，再让 `reqwest` 在连接时重新解析。被检查的地址没有绑定到真实 socket，攻击者控制的域名可以在两次解析之间从公网地址切换到 loopback/private/metadata 地址。系统代理还可能在代理端重新解析目标名，绕开本地检查。

该问题不依赖 tool hash、transcript marker 或 Claude CLI 版本；它是所有开启远程 source 下载的 Messages 与 `count_tokens` 路径共有的辅助 I/O 和资源边界问题。

## 根因与源码链

修复前调用链：

```text
Messages/count_tokens JSON
  -> prepare_multimodal_message_sources
  -> materialize_remote_multimodal_sources
  -> 对每个 message/content 串行 materialize_content_sources
  -> ensure_safe_remote_url_resolves（第一次 DNS）
  -> reqwest::Client.get().send（连接时第二次 DNS/可能使用系统代理）
  -> 每源独立 20 MiB Vec
  -> base64 String 写回 payload
```

根因分为四类：

1. 只限制单个响应，没有请求级 source count、累计 decoded bytes、累计 base64 bytes 和总 HTTP attempt。
2. `Client::timeout(25s)` 是单个 source/request 的超时，不是整个物料化工作流的 deadline。
3. DNS 安全检查和 socket 使用的解析结果不是同一个结果，属于典型 TOCTOU/rebinding 窗口。
4. 没有跨请求的远程物料化准入，突发流量会同时放大辅助 HTTP 和内存工作集。

历史架构审计已记录同一事实：`SEC-001` 与 `RES-001`，见 [当前资源模型](../../docs/plantree/baseline/resource-and-concurrency-model.md)；本专题把它提升为当前 Rust 版本的实施与发布阻断项。

## 复现方案

### 最小静态复现

1. 在一个 Messages body 中放入多个很短的 URL source。
2. 每个源返回小于 20 MiB 的数据。
3. 观察旧实现逐个发送请求并把每个结果保留为 base64；总请求数和总字节没有请求级上限。

### DNS rebinding 复现

使用确定性 resolver：第一次查询返回公网地址，第二次查询返回 `127.0.0.1` 或 RFC1918 地址。旧链的独立预检查会接受第一次结果，transport 可以使用第二次结果。验收不能只调用 URL 校验函数；必须证明 transport 的 resolver 返回 blocked address 时 socket 端 0 accept。

### 多轮与异常复现

- 21 个远程 source，目标主机均设置命中计数；应在 DNS/HTTP 前拒绝，命中数为 0。
- 多个 chunked source 单个均小于 20 MiB，但累计超过请求上限；应在累计边界停止读取。
- redirect 链跨 source 消耗同一个 attempt budget；预算耗尽后不得再发送下一跳。
- 公网形态域名 redirect 到 loopback/private literal；第二跳 HTTP 命中必须为 0。
- 慢 body/无 terminal body 超过工作流 deadline；future 被取消，随后正常请求 5/5 恢复。
- 并发远程 Messages 与 `count_tokens` 超过全局工作槽；峰值工作流不得超过硬上限，释放后立即恢复。
- 正常 base64、data URL、file source 和单个支持的远程 PNG/JPEG/WebP/GIF/PDF/text 逐类至少 5 轮，确认未误入网络或未被错误改写。

## 修复与优化方案

已选方案：

- 在构建 HTTP client 和 DNS 查询前线性预扫描所有 messages/content，远程 URL source 上限为 20；data URL 不计入远程 source。
- 保留已有单源 20 MiB 能力；新增每请求累计下载 32 MiB、累计 base64 44 MiB、HTTP/redirect attempt 32 次。
- 保留每源最多 5 次 redirect，并让所有 redirect/source 共享同一个 request budget。
- 整个远程物料化工作流使用 45 秒 deadline；单个 HTTP request 保留 25 秒上限。
- 使用 `reqwest::dns::Resolve` 包装真实 transport resolver；每次连接解析得到的全部地址都必须为允许的公网地址，blocked/mixed answer fail closed。
- 下载 client 使用 `no_proxy()`，避免系统 HTTP/SOCKS 代理在远端重新解析目标主机；每个 redirect 重新验证 scheme、host、port、userinfo 和 literal IP。
- URL 禁止 credentials、port 0、localhost/metadata 名称及 trailing-dot 变体，并补齐 benchmark/reserved/IPv6 transition 等非公网范围。
- 进程内同时最多 4 个远程物料化工作流；permit 随 parsed-body report 保持到 handler 构造最终 response，`count_tokens` 保持到 tokenization 完成。
- 报告记录 remote source count、downloaded bytes、materialized bytes 和 HTTP attempts，便于后续 usage/metric 接入。

未选方案：继续执行两次 DNS 再比较。它仍不能保证 transport 实际只连接被检查地址；在 retry、Happy Eyeballs、连接池和 proxy 下也无法建立可靠绑定。

## 验收、测试与当前证据

当前聚焦测试命令：

```bash
cargo +1.92.0 test anthropic::body_processing::tests -- --nocapture
```

当前结果为 19/19 通过；新增异常场景均执行 5 轮：source count 预拒绝、inline 排除、attempt/下载/base64 边界、实际 transport blocked-address 0 connect、每次 DNS lookup 过滤、支持的单远程 PNG、chunked aggregate over-limit、redirect 到私网、redirect attempt 共用、slow cancel/recovery、全局工作槽耗尽/恢复、URL credentials/reserved range。另对 1 KiB/100 KiB/1 MiB/5 MiB clean text 各执行 100 轮 value-identity 与 remote-admission=0 测试。

构建身份和逐项摘要见 [远程多模态证据](../evidence/remote-multimodal-resource-and-ssrf-20260716.md)。handler 门禁另覆盖五个 Messages 与五个 `count_tokens` 路径，各 5 轮 remote over-count，共 50/50 规范 400、request ID 一致、0 inference hit；inline data URL/base64 对照 30/30 正常。当前证据仍不替代以下发布门禁：

- 20 source/32 attempt 的临界组合和并发 burst RSS/FD/连接峰值；
- 真实 release binary 下图片/文档单点与 Claude CLI image；
- L3/L5 取消、恢复和三轮 soak；
- 最终统一候选 SHA 重跑。

## 性能边界

正常没有远程 URL 的请求只增加一次线性 content-block 计数；source count 为 0 时不构建 reqwest client、不获取 semaphore、不做 DNS/HTTP。inline base64/data URL 保持原处理路径。当前 debug 聚焦测试中，四档 clean text 各 100 轮的 p95 为 0-3 微秒，value identical；该数字只证明没有隐藏 serialize/clone，最终性能结论仍以 release B05/L5 为准。

远程请求的额外安全成本发生在实际 transport DNS resolver 中，不再支付旧的“预解析一次 + transport 再解析一次”。全局 4 工作流会在突发时形成有界等待，等待时间受同一个 45 秒 deadline 约束，不会形成无限队列。

发布前仍需记录 1/20 source、1/4/8 并发下的 p50/p95/p99、RSS/FD 起峰终值、HTTP hit 数和 deadline recovery；若正常单 URL p95 明显回退，优先优化 client/resolver 复用，但不能恢复双 DNS 或移除硬预算。

## 残余风险、回滚与限制

- `no_proxy()` 会改变依赖系统代理下载远程 source 的部署行为。这是 SSRF 连接绑定的安全取舍；不能通过恢复透明代理来回滚。需要代理的部署必须以后实现显式、受信代理策略及代理端目标地址证明。
- 当前全局准入是固定工作流数，不是根据容器内存动态计算的完整 weighted resource governor；当前请求级上限使风险有限，但 release load gate 仍需证明目标部署的 RSS envelope。
- DNS/HTTP 不能证明远端内容本身可信；media type、PDF 解码与后续 tokenizer 仍需各自的字节/CPU/阻塞池上限。
- 回滚点应是本批修复前的工作提交。若兼容性问题只涉及数量/时间阈值，可在新证据支持下调整阈值；不得回滚 resolver 绑定、私网拒绝、累计预算或 deadline 的存在。
