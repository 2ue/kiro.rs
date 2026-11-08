# Round 67：thinkingControls 提示词 A/B

## 目的

验证 `/dfcache/tune-ss6-99` 的 `thinkingControls` 注入是否只是影响评分，还是请求协议的必要条件。

## 操作与结果

- 先只读保存 59139 Admin runtime 配置：`/tmp/kirotest-dev/artifacts/runtime-before-round67.json`
- 仅修改开发实例 `/dfcache/tune-ss6-99` 的 `promptInjection.pathOverrides.*.thinkingControls.enabled` 为 `false`
- 未修改 59138、Caddy、生产账号或生产数据库
- cctest 首项即失败，结果文件：`results/20261107T173429-round-67-thinking-controls-off.json`
- CCTest 错误：`400 invalid_request_error`
- 错误正文：`reasoning was requested, but both native reasoning fields and compatible thinking prompt controls are unavailable`
- 抓包：`logs/llm-capture/capture-20261107T173429Z-df4e20da/events.jsonl`
- 事件行：24；6 个请求全部在本地 preflight 被拒绝；没有上游请求
- 原始归档：`/tmp/kirotest-dev/artifacts/round-67-capture.zip`

测试完成后已用原始 runtime JSON 恢复 `thinkingControls.enabled=true`，恢复响应确认值为 `true`；59138/59139 `/healthz` 均为 HTTP 200。

## 结论

`thinkingControls` 不是当前 82 分失败项的可选语义提示，而是请求声明 reasoning、且没有原生 Kiro reasoning 字段时的兼容协议前置条件。关闭它会导致本地 400，不能以该方式测试评分。当前证据不支持删除或清洗该注入；下一步仍应围绕上游 EOF、真实签名重放和 WebSearch opaque 字段取证。
