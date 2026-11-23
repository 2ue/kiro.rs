# 当前 Codex 会话任务提取

本文根据本机 Codex CLI 会话
`01a0d164-9788-76a1-a836-aa5447944ece` 的 JSONL 记录整理。会话文件为：

`../../../.codex/sessions/2026/09/24/rollout-2026-09-24T11-10-25-01a0d164-9788-76a1-a836-aa5447944ece.jsonl`

会话工作目录是 `.`。用户在这条会话中反复发送“继续”，这些消息不是新任务，而是要求继续完成下面这条未闭环的主任务。

## 主任务

主任务是分析 `152.53.243.159` 上仅监听宿主端口 `59137` 的 kiro.rs 服务，找出本地 Kiro 账号并发槽位持续占用、请求排队和长耗时的根因，并建立能够在下一次异常发生的第一时间固化证据的独立采集系统。

分析必须只覆盖本地 Kiro 账号，不分析外部池账号；不能因为分析、抓包或诊断影响 `59137` 的正常调用、调度、流式响应或并发释放。

## 根因分析要求

需要用 usage 原始记录、Redis 调度状态、数据库、服务状态和多条真实请求的时间线，把以下时间拆开：

- 调度排队等待时间；
- credential lease 获取时间；
- 上游 HTTP response headers 到达时间；
- 第一个上游 body chunk 时间；
- 第一个下游可见输出时间；
- 每个上游 frame 的到达和解析时间；
- EOF、显式完成事件、idle timeout、客户端断开和 lease release 时间。

不能把 usage 的总 `duration_ms` 直接当成排队时间，也不能把“曾经观察到高并发”当成“当前槽位泄漏”的证明。必须回答：

1. 长请求是正常的深度 reasoning/慢上游，还是异常流量；
2. 上游是否真的缺少结束标识，是否存在未知事件或协议形态变化；
3. 正常请求是否因为解析错误被判失败；
4. 失败请求是否被错误判成功；
5. EOF、显式 `COMPLETED`、HTTP/2 `END_STREAM`、RST/GOAWAY 和本地 stream close 的关系；
6. lease 是在正常结束时释放，还是存在客户端断开、解析错误或异常 EOF 后未释放；
7. 本地账号持续占满时是否会影响外部池，二者是否隔离，以及 fallback 是否会把请求错误地跨池转移。

现有文档 `docs/analysis/prod-59137-local-queue-root-cause-20260924.md` 已保存第一轮生产证据，但这份证据主要证明“长流占槽形成容量压力”，还不能证明某一种具体协议结束标识变化就是根因。下一次异常时必须立即开始采集，不能等流量恢复正常后才采集。

## 独立采集器任务

采集器必须是独立服务，监听 `19137`，目标服务继续监听 `59137`。采集器不能替换、重启或修改 `59137` 的业务容器。

采集器需要具备：

- 默认关闭采集；
- 页面开关控制开始、暂停、继续、停止；
- 中文页面；
- 实时 terminal 风格事件日志，不使用百分比或目标进度条；
- 实时显示包数量、字节数、TCP 会话、UDP 流、活跃连接、时间和采集阶段；
- 采集完成列表显示开始时间、结束时间、持续时间、大小和分页；
- 支持下载 ZIP 和直接可读的纯文本报告；
- 没有匹配到数据时给出明确提示且不生成伪造采集包；
- 路径遍历、未授权控制和下载必须安全；
- 采集内容包含 pcap、运行状态快照、容器状态、配置摘要、只读 Admin 状态、日志、采集命令、事件日志和分析报告；
- 只抓配置的上游端口，不抓其他服务；
- 同时覆盖 IPv4/IPv6、TCP/TLS 和 UDP/QUIC，避免 HTTP/3 或地址族变化造成“没有流量”的假阴性；
- 报告必须明确说明采集窗口边界、缺失 SYN/FIN/RST、UDP 无关闭标志，以及这些事实不能单独证明应用层是否结束。

相关实现范围是：

- `tools/kiro-capture-sidecar/server.py`
- `tools/kiro-capture-sidecar/test_server.py`
- `tools/kiro-capture-sidecar/README.md`
- `scripts/diagnostics/capture_container_tls.py`
- `scripts/diagnostics/packet_protocols.py`
- `scripts/diagnostics/analyze_pcap_flows.py`
- `scripts/diagnostics/decrypt_tls_http2.py`

## HTTPS 明文和完整对话的边界

用户要求尽可能看到每次大模型调用的完整内容，包括 system prompt、用户消息、工具定义、工具输入输出、thinking/reasoning、模型输出和 Kiro EventStream。

旁路 pcap 在没有 TLS 会话密钥时只能看到密文、连接时序、字节数、包间隔和 TCP/UDP 终止信息，不能还原 HTTPS 正文。现网之前没有配置 `KIRO_TLS_KEYLOG_FILE`，独立采集器也没有自动获得会话密钥。

因此必须区分两种能力：

1. **无密钥的独立旁路采集**：可以证明网络连接、流量方向、长间隔、FIN/RST/GOAWAY 等传输层事实，但不能看到 system prompt 或模型正文。
2. **有密钥的离线解密**：目标进程为同一批 TLS 会话产生 NSS keylog，采集器使用 `tshark` 和 `decrypt_tls_http2.py` 离线还原 HTTP/2 请求正文、响应 EventStream、事件类型、状态字段和 JSON payload。

市场上的抓包软件也不能凭空从 TLS 密文恢复正文。若不改目标服务源码，必须让目标进程产生 keylog，或把流量经过独立代理/MITM 并让目标信任代理证书。不能把“抓到 pcap”误报成“抓到了对话明文”。

会话中曾经临时增加 Rust 应用侧 frame 记录器，后来用户明确纠正范围：目标是改独立采集系统，不是把完整对话采集强耦合进 kiro.rs。生产路径没有接入这个记录器，因此不能把它当成已完成能力。

## 会话结束时的实际进度

已经完成或基本完成：

- `19137` 独立采集器的页面、控制接口、分页列表和直接可读文本报告；
- pcap 捕获和增量实时进度；
- TCP/UDP/IPv4/IPv6 解析和传输流报告；
- HTTP/2 解密器和 AWS EventStream 解析器；
- `END_STREAM`、RST_STREAM、GOAWAY、请求/响应方向分离等终止证据；
- 缺少 keylog 或 tshark 时的清楚降级说明；
- 无数据不生成采集包；
- 采集器测试、pcap 测试和解密器测试。

会话最后停在一次 UDP/QUIC 进度测试和文案一致性收尾上。当前本地重新运行结果为：

- 诊断脚本测试：12 项通过；
- 独立采集器测试：16 项通过；
- 总计：28 项通过；
- 未发现测试失败。

本轮已经补齐并验证了会话最后停下的本地收尾，并把独立采集器更新同步到远端：

- 远端 `kiro-capture-sidecar.service` 已重新加载并为 `active`；
- 监听地址为 `0.0.0.0:19137`；
- `59137` 的容器重启计数仍为 `0`，镜像仍为 `kiro-rs:0.0.169-keylog-20260925`，健康状态为 `healthy`；
- `59137/healthz` 返回 `ok`，`59137/readyz` 的 PostgreSQL、Redis 和 Redis runtime events 检查均为 `true`；
- 页面检查通过，已包含 terminal 日志和 TCP/UDP 流显示，没有百分比进度条；
- 远端 API 当前明确报告没有 TLS keylog、没有 `tshark`，因此 HTTPS 正文仍不可见。

仍未闭环：

1. 现网没有配置覆盖同一批新 TLS 连接的 keylog，远端采集包仍不能自动显示真实 HTTPS 对话；
2. 本机和远端当前都没有完成真实 TLS pcap/keylog 到明文报告的端到端验证；
3. 没有在下一次真实“并发槽位持续打满”窗口中同时取得 usage、lease、运行状态、pcap 和明文 frame 的多请求对照证据；
4. 不能仅凭当前正常窗口的采集包下结论说已证明异常根因。

## 下一步执行顺序

1. 只对独立采集器和诊断脚本做本地收尾，保持 `59137` 业务代码和协议处理不变。
2. 在远端只更新并重启 `kiro-capture-sidecar.service`，确认 `19137` 可访问，同时核对 `59137` 健康状态和重启计数不变。
3. 明确选择 HTTPS 明文方案：配置目标进程产生 NSS TLS keylog，或部署独立 HTTPS 代理/MITM；单纯旁路 pcap 不能完成正文采集。
4. 下一次异常一出现就从页面启动采集，保存多个本地账号请求的完整窗口，不能只抓一个请求，也不能等异常结束后再抓。
5. 将采集包中的 runtime snapshot、usage 时间线、lease 状态、pcap 流和解密后的 EventStream 按 request ID/时间关联。
6. 只有当多条请求证据显示协议实现确实不完整时才改协议；若协议正确，则优先采用通用的有界等待、连接终止、lease 生命周期和调度隔离改进，不能误杀正常请求。

## 验收标准

- 正常长请求仍能成功完成；
- 正常 reasoning 不因“首个可见输出很晚”被误判失败；
- 真正异常 EOF、解析失败、未知协议和客户端断开有明确分类；
- lease 在 EOF、错误、idle timeout、stream drop 和客户端取消路径都最终释放；
- 采集器关闭时不影响 `59137`；
- 采集器打开时不阻塞请求主路径；
- 采集结果无需专用软件即可先阅读；
- 有 keylog 和 tshark 时才能声称看到了 HTTPS 正文；
- 证据不足时报告必须明确写“不可判定”，不能把推测写成根因。
