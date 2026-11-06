# 错误说明

接口出错时返回 Anthropic 兼容的错误结构：

```json
{
  "type": "error",
  "error": {
    "type": "api_error",
    "message": "The request could not be completed. Please try again. (request-id: req_01...)"
  },
  "request_id": "req_01..."
}
```

- `message` 只用英文，末尾的 `(request-id: ...)` 是排查编号，与响应体 `request_id` 字段、`request-id` 响应头一致，反馈问题时附上即可。
- 只要本次请求失败，HTTP 状态码就不会是 200，失败的请求不计费。
- 唯一的例外：流式响应已经开始输出内容之后才中断。此时状态码已经是 200，错误会以流里的 `event: error` 事件结束；这类请求在中断前确实收到过内容。

响应头 `x-should-retry: false` 表示原样重试不会成功，需要先调整请求。

## 错误一览

| 状态码 | type | message | 原因 | 调用方怎么做 |
|---|---|---|---|---|
| 502（可调整为 503 / 529） | api_error（529 时为 overloaded_error） | The model returned an empty response. Please try again. | 模型返回成功但整条回复没有任何文字、思考或工具调用；服务端已自动换线路重试 | 重新发送请求；频繁出现时稍等再试 |
| 502 | api_error | The request could not be completed. Please try again. | 请求已发出但没有正常完成 | 稍后重试 |
| 503 / 504 / 529 | api_error / overloaded_error | The model service did not respond in time. Please try again. | 网络超时、连接中断或服务繁忙 | 稍后重试 |
| 200（流内 error 事件） | overloaded_error | The response was interrupted before it finished. Please try again. | 已经输出部分内容后连接中断 | 重新发送请求；客户端（如 Claude Code）通常会自动重试 |
| 429 | rate_limit_error | Too many requests right now. Please try again shortly. / Too many requests right now. Please try again in N seconds. | 请求频率过高或处理容量暂时用满 | 按提示的秒数后重试 |
| 503 | api_error | The service is starting up. Please try again shortly. | 服务刚启动或正在切换 | 稍后重试 |
| 400 | invalid_request_error | The request could not be processed. Please check the message history, tool results, files, or images and try again. | 请求结构不被接受 | 检查历史消息、工具结果或附件；原样重试不会成功 |
| 400 | invalid_request_error | `prompt is too long: ...` | 输入超过长度上限 | 压缩上下文（如 `/compact`）或新开对话。此文案保持英文，Claude Code 依赖它自动压缩 |
| 400 | invalid_request_error | One or more images exceed the 5 MB size limit. Please resize or remove them and try again. | 单张图片过大 | 压缩或移除图片 |
| 400 | invalid_request_error | The request contains more than 100 images. Please send fewer images and try again. | 图片数量过多 | 减少图片数量 |
| 400 | invalid_request_error | An image format is not supported. Please convert it to PNG, JPEG, GIF, or WebP and try again. | 图片格式不支持 | 转换格式后重试 |
| 400 | invalid_request_error | 模型给出的英文原因说明（原文） | 参数不被所选模型支持，例如要求特定的 thinking 设置 | 按说明调整参数 |
| 404 | not_found_error | The requested model is not available: <model> | 模型名错误 | 更换模型 |
| 400 | invalid_request_error | The requested model is not available on this endpoint. Please choose another model. | 该接口不提供这个模型 | 更换模型 |
| 502 | api_error | Web search failed. Please try again later, or disable web search. | 联网搜索工具失败 | 稍后重试或关闭联网搜索 |

## 空回说明

「空回」只按整条回复判断：回复结束时从头到尾都没有任何文字、思考或工具调用才算。回复过程中某一段暂时没有内容、长时间思考后才开始输出，都不算空回，会正常返回。

空回时服务端会先自动换线路重试，全部失败后才返回上面第一行的错误，不计费。

## 文案约定

- 对外 `message` 一律英文，不出现中文、内部组件、账号或渠道名称；末尾统一附 `(request-id: ...)`。
- 文案集中在 `src/anthropic/envelope.rs`，不做成配置。
