# P004 证据：raw 顶层 `max_tokens` 校验

## 运行身份

| 字段 | 值 |
| --- | --- |
| 日期 | 2026-09-02 |
| 服务 | `kiro.rs` |
| 地址 | `127.0.0.1:19023` |
| PID | `81106` |
| 配置 | `tmp/thinking-budget-local/config.json` |
| 二进制 | `/tmp/kiro-thinking-candidate.dlJWU9/kiro-rs` |
| SHA-256 | `d7764aea6ea97abe55decfd182732db771f719fe51f60462488f1c5fb543b623` |
| 账号路径 | local credential |
| 外部池 | 本轮未触发 |

## HTTP 结果

每个请求均使用 `POST /cc/v1/messages` 和本地测试 API key。请求体只包含一个
短文本消息；没有记录凭据或完整账号信息。

| Case | HTTP | Request ID | 下游错误 |
| --- | ---: | --- | --- |
| `max-null` | 400 | `req_01Qu1F95RHQdZPmjrT7a8yuo` | `max_tokens must be an integer` |
| `max-float` | 400 | `req_018b6GVZbSGmcQmm5ordPg5c` | `max_tokens must be an integer` |
| `max-zero` | 400 | `req_01H2g9VYAwVKLb9u4zWhJU7Q` | `max_tokens must be between 1 and 2147483647` |
| `max-negative` | 400 | `req_01954Eq2zU6n1F6edkfsnHjZ` | `max_tokens must be between 1 and 2147483647` |
| `max-overflow` | 400 | `req_01iHGQsPsmhYYLiMqt6M79At` | `max_tokens must be between 1 and 2147483647` |

## 结论

该类错误现在在请求入口被确定性处理：

- 不会把无效格式发送给 Kiro 上游；
- 不会进入 thinking-signature rescue；
- 不会消耗 inference retry budget；
- 不会盲目换账号或切到 external pool；
- 客户端得到可定位的字段错误，而不是泛化的上游失败。

原始脱敏响应文件位于：

`tmp/thinking-budget-local/evidence/current-regression-20260902/`

## 源码和门禁

新增单元测试：

`raw_reasoning_protocol_rejects_invalid_top_level_max_tokens_for_all_routes`

结果：

```text
9 passed; 0 failed
git diff --check: passed
release-gate result=pass
```
