# 远程多模态资源与 SSRF 聚焦证据

Date: 2026-07-16

Status: `focused-pass / unified-candidate-and-load-pending`

## 构建身份

- Git HEAD: `401473c`；工作树包含本轮未提交修改。
- Rust toolchain: `1.92.0`。
- 测试入口：`src/anthropic/body_processing.rs`。
- 早期隔离复核曾记录活跃生产端口 `127.0.0.1:9022` 的 PID 未变化；该 9022 listener 探针按当前安全合同不再作为 release 证据。当前同类 runner 必须只按数值排除 9022，不读取既有 listener。
- 测试使用进程内临时 loopback listener、固定 resolver 和 `media.test` override；每个 listener 随测试 task abort 清理。

最终 release candidate 必须补 Git diff hash、release binary SHA-256、端口/PID、RSS/FD 和负载报告；本记录不把 dirty debug test binary 当作发布候选。

## 命令与结果

```bash
cargo +1.92.0 check --tests
cargo +1.92.0 fmt --check
cargo +1.92.0 test anthropic::body_processing::tests -- --nocapture
```

结果：

- `check --tests`: PASS；全局仍有与本专题无关的既有 dead-code warnings，不能据此关闭 Clippy 发布门禁。
- `fmt --check`: PASS。
- body processing: 19/19 PASS；新增异常/恢复 case 内部各运行 5 轮；clean text 四档各 100 轮。
- `all_multimodal_handlers_reject_21_remote_sources_before_upstream_for_five_rounds`: PASS；10 路由 x 5 轮 remote=50/50，inline controls=30/30。
- 完整 `anthropic::handlers::tests`: 90/90 PASS。

## 覆盖事实

| 类别 | 证据 | 结果 |
| --- | --- | --- |
| 预扫描 | 21 URL source，域名不可解析 | 在 client/DNS/HTTP 前稳定拒绝 |
| 正常 inline | data URL + 一个 remote URL | 只计一个 remote source |
| 请求预算 | attempt、download、materialized 边界及 `+1` | 边界接受，超一稳定拒绝，计数不越界 |
| DNS rebinding | 同一 host 第一次公网、第二次 loopback | 第一次通过，第二次由 transport resolver 拒绝 |
| 实际 socket | resolver 只返回 loopback，真实 reqwest send | 5/5 error，listener 0 accept |
| 正常远程图片 | 临时 HTTP server 返回 PNG | 5/5 media type/base64/value identical，1 hit/request |
| chunked over-limit | 无 Content-Length，分块累计越界 | 5/5 在累计边界终止，0 materialized output |
| redirect SSRF | `media.test` redirect 到 `127.0.0.1` | 5/5 只命中第一跳，第二跳 0 HTTP |
| redirect RPM | 三跳链、request attempt 上限 2 | 5/5 总 hit=2，不发送第三跳 |
| deadline/recovery | 200 ms slow body、20 ms test deadline，再发 normal | 5/5 cancel，随后 5/5 normal 恢复 |
| 全局准入 | 同时持有全部 4 个 permit，再申请第 5 个 | 5/5 fail closed；释放后立即恢复 |
| URL 语义 | credentials、localhost trailing dot、IPv4 shorthand、benchmark IPv4、doc IPv6、port 0 | 全部拒绝；已列公网 IPv4/IPv6/域名接受 |
| clean text identity | 1 KiB/100 KiB/1 MiB/5 MiB，各 100 轮 | value identical、0 remote hit/attempt/permit；debug p95 分别 3/1/1/0 微秒 |
| handler remote preflight | `/v1`、`/cc`、`/na`、`/ha`、`/dfcache/demo` 的 Messages 与 count_tokens，各 5 轮 21 URL | 50/50 Anthropic 400；header/body request ID 一致；无 URL 回显；0 inference hit；单请求 <=1s |
| handler inline controls | 五个 count_tokens 各 5 轮 21 data URL + 21 base64；`/v1/messages` 5 轮 | 25/25 count_tokens 200/positive tokens/0 inference；5/5 Messages 200/有效 EventStream/5 inference hits |

## 尚未执行

- 真实外网 URL；安全结论不依赖真实外网，兼容性将在低量 release-candidate smoke 中验证。
- L3 burst、L5 soak、RSS/FD/TTFB 分位。
- PDF/text document、Claude CLI image、取消时的客户端可见 error envelope。
- 统一最终候选重新构建与 SHA 绑定。

以上均保持发布 `NO-GO`，详见 [专题](../issues/remote-multimodal-resource-and-ssrf-bounds.md) 与 [重新验证矩阵](../tests/reverification-matrix.md)。
