# Kiro 兼容图片转发与外部池模式分析

日期：2026-07-05

## 背景

本分析记录针对的问题是：

- 下游输入可能来自 Claude Code CLI / Anthropic 兼容协议，图片可能以内联 base64、data URL、`file_id`、远程 URL 等形式出现。
- 上游可能是本地 Kiro 凭证，也可能是外部池。
- 用户期望确认是否可以“转换成 Kiro 兼容格式后透传图片，本地不做处理”。
- 需要同时分析本地凭证模式和外部池模式，而不是只看 `/cc/v1` 或单一路径。

这里的核心歧义是“本地不做处理”有两种含义：

1. 不长期保存图片、不缩放、不读取尺寸、不 decode 图片字节、不做媒体类型修正。
2. 完全不解析 body、不转换 schema、不重新序列化，byte-to-byte 透传。

结论是：

- 第一种可以通过轻量模式实现。
- 第二种在本地 Kiro 凭证模式下做不到，因为 Anthropic/Claude 输入协议和 Kiro 上游协议不是同一个 JSON schema。
- 外部池能否接近原始透传，取决于外部池上游是 Anthropic 兼容协议还是 Kiro 兼容协议。

## 当前协议差异

### Anthropic / Claude 输入图片格式

下游常见图片块如下：

```json
{
  "type": "image",
  "source": {
    "type": "base64",
    "media_type": "image/png",
    "data": "..."
  }
}
```

Claude Code CLI 的图片读取结果也可能嵌在 `tool_result.content[]` 中，本质仍然是 Anthropic 风格的 content block。

### Kiro 本地上游图片格式

Kiro 请求里的图片不是放在 `content[].source`，而是放在 `userInputMessage.images[].source.bytes`：

```json
{
  "userInputMessage": {
    "content": "...",
    "images": [
      {
        "format": "png",
        "source": {
          "bytes": "..."
        }
      }
    ]
  }
}
```

代码证据：

- `src/kiro/model/requests/conversation.rs`
  - `UserInputMessage.images: Vec<KiroImage>`
  - `KiroImage { format, source }`
  - `KiroImageSource { bytes: Option<String> }`

因此，只要上游是 Kiro 本地凭证，就必须把 Anthropic 图片 block 转成 Kiro 的 `images[].source.bytes`。这不是可选处理，而是协议转换。

## 本地凭证模式分析

### 当前链路

本地凭证模式大致链路是：

```text
下游 Anthropic / Claude JSON
  -> parse_messages_payload
  -> file_id 物化
  -> 远程图片 URL 物化
  -> base64 图片 media_type 修正
  -> Anthropic -> Kiro schema 转换
  -> Kiro request JSON 序列化
  -> endpoint transform / whitespace compression
  -> Kiro 上游
```

关键代码位置：

- `src/anthropic/handlers.rs`
  - 消息请求中会调用 `files::materialize_file_sources(...)`
  - 会调用 `materialize_remote_multimodal_sources(...)`
  - 会调用 `normalize_base64_image_media_types(...)`
- `src/anthropic/converter.rs`
  - `convert_image_source(...)` 将 Anthropic image source 转成 `KiroImage::from_base64(...)`
- `src/kiro/provider.rs`
  - 最终会将 Kiro request body 变成完整字符串/bytes 后用 reqwest `.body(body)` 发送

当前并不是纯转发，而是为了让 Kiro 上游能稳定识别图片，做了完整标准化。

### 当前 inline base64 图片是否会长期存储

inline base64 图片不会因为消息请求本身被长期保存到文件 store。它主要存在于：

- 原始 HTTP body。
- 解析后的 `MessagesRequest`。
- 转换后的 Kiro request 结构。
- 序列化后的 Kiro JSON body。

所以 inline base64 场景不是“上传后存进本地文件池”，但也不是零拷贝。当前架构会产生多份内存副本。

### 当前 file_id 图片是否会存储

会。

为了兼容 Anthropic Files API，当前实现有本地 `AnthropicFileStore`：

- `src/anthropic/files.rs`
  - `StoredFile.bytes: Arc<Vec<u8>>`
  - `AnthropicFileStore.inner`
  - 上传文件会进入内存 store
  - 消息中出现 `source.file_id` 时，再从 store 取出 bytes 并转成 base64

原因是 Kiro 本地上游不认识 Claude/Anthropic 的 `file_id`。如果下游只传 `file_id`，代理必须在某个地方能拿到真实 bytes，否则无法构造 Kiro 的 `source.bytes`。

所以 file_id 场景下不能做到“完全不存储”，除非：

- 禁用 file_id，只允许 inline base64。
- 把 store 从进程内存改成磁盘/Redis/S3。
- 或者把 `/files` 上传也转发到同一个支持 files API 的上游，并确保后续消息粘到同一个上游。

最后一种要求上游协议和调度都支持，当前不能直接假设成立。

### 当前哪些处理会产生额外 CPU / 内存开销

本地凭证模式下，和图片相关的主要开销包括：

1. JSON parse。
2. `MessagesRequest` 结构持有 base64 字符串。
3. `file_id` 物化时读取本地 store 并 base64 encode。
4. 远程 URL 图片物化时下载图片并 base64 encode。
5. `normalize_base64_image_media_types` 会 decode base64 来识别 PNG/JPEG/GIF/WEBP 头。
6. `convert_image_source` 会归一化 data URL/base64，并生成 Kiro image。
7. token 估算可能读取图片尺寸或回退为默认图片 token。
8. payload guard 可能扫描图片 source 大小。
9. Kiro request 序列化会再产生完整 JSON body。
10. endpoint transform / JSON whitespace compression 会产生额外字符串处理。

其中最需要注意的是：

- media type 修正会 decode base64。
- file_id 物化会产生从 bytes 到 base64 的转换。
- 远程 URL 物化会下载并持有图片。
- 完整 JSON 序列化会让大图片请求出现额外内存副本。
- payload guard 和 token 估算虽然不是图片转发本身，但在长上下文、多图、大并发下会进入 CPU 热路径。

### 本地凭证能否做到 Kiro 兼容“轻量转发”

可以做到部分轻量化，但不能完全 byte-to-byte 透传。

可行的轻量化定义：

- 对 inline base64 图片，只做 schema 转换。
- 不 decode 图片字节。
- 不读取图片尺寸。
- 不修正 media type。
- 不缩放、不压缩、不落盘。
- 直接把下游 `source.data` 搬到 Kiro `source.bytes`。
- 使用 `media_type` 或 data URL metadata 推导 Kiro `format`。
- 缺少 media type 时直接报错，或者走默认拒绝策略，不再 decode 猜测。

不可避免的处理：

- 必须解析 JSON。
- 必须转换 Anthropic schema 到 Kiro schema。
- 必须重新序列化 Kiro JSON。
- 必须持有完整请求体或完整上游 body。

因此，对本地 Kiro 凭证更准确的表述是：

```text
可以做“图片不解码、不落盘的 Kiro schema 转换转发”；
不能做“完全不处理、不转换的图片透传”。
```

### 本地轻量模式的风险

如果信任下游 `media_type`，可能出现：

- 下游声明 `image/png`，实际是 JPEG，Kiro 上游拒绝。
- 下游 base64 非法，代理不提前发现，错误延迟到 Kiro 上游。
- Claude CLI 的 `count_tokens` / autocompact 估算更粗。
- payload guard 如果完全关闭，超大图片可能把本地内存或 Kiro 请求体打爆。

所以即使做轻量模式，也建议保留这些最小保护：

- 请求体总大小限制。
- 单张图片 base64 字符串长度限制。
- 图片数量限制。
- file_id store 总量限制，或者直接禁用 file_id。
- 明确的错误提示，避免上游返回模糊 400。

## 外部池模式分析

### 当前外部池链路

当前外部池并不是 Kiro body 模式。

`ExternalRouteRequest` 同时保存：

- `raw_body: Bytes`
- `payload: MessagesRequest`

但实际发送外部池前，`external_pool_prepare_request(...)` 会优先把 `route.payload` 转成 JSON value，然后做：

- outbound model 映射。
- thinking 字段归一化。
- 重新序列化为 JSON body。

也就是说，当前外部池默认链路是：

```text
下游 Anthropic / Claude JSON
  -> parse MessagesRequest
  -> 可能被 file_id / remote URL / media_type 标准化
  -> payload 重新序列化
  -> Anthropic 风格 body 发外部池
```

当前不是：

```text
下游 Anthropic / Claude JSON
  -> 转 Kiro JSON
  -> 发外部池
```

### 外部池分两种上游类型

外部池必须先区分上游协议类型，否则“图片直传”会混乱。

#### 1. Anthropic 兼容外部池

如果外部池上游本身是 Anthropic 兼容 API，那么不应该把请求转成 Kiro 格式。

合理目标应该是：

```text
下游 Anthropic body
  -> 尽量原样 / 轻量修改
  -> 外部 Anthropic 兼容上游
```

图片仍然保持：

```json
{
  "type": "image",
  "source": {
    "type": "base64",
    "media_type": "image/png",
    "data": "..."
  }
}
```

这种模式最适合做接近真实的 raw passthrough：

- 不转 Kiro schema。
- 不 decode 图片。
- 不修正 media type。
- 只处理必要的 model mapping、header、超时、usage 整形、错误统一。

但当前实现需要注意一个问题：

消息 handler 中外部 fallback/direct context 创建后，后续仍会执行 file_id 物化、远程图片 URL 物化、media type 修正，然后 `external.refresh_payload(&payload)`。这意味着外部池直连当前收到的是标准化后的 payload，不是严格原始 body。

如果目标是“外部池直连时完全不处理图片”，需要调整外部池直连的进入时机，或者增加外部池级别的 request body mode。

#### 2. Kiro 兼容外部池

如果外部池上游本身是 Kiro 兼容协议，那当前没有完整独立模式。

不能只把请求体改成 Kiro JSON，因为还要同步处理：

- Kiro 请求体构造。
- Kiro eventstream 或非流式响应解析。
- Kiro -> Anthropic SSE/JSON 输出转换。
- usage 字段映射。
- 错误格式统一。
- 首字延迟和流式事件顺序。
- 计费模型。
- 路径缓存策略。
- 外部池调度和健康检查。

否则会出现请求是 Kiro 格式、响应却按 Anthropic 处理的错配。

因此如果要支持 Kiro 兼容外部池，建议新增明确协议类型：

```text
externalPool.protocol = anthropic | kiro
```

并围绕该协议类型分别处理请求和响应，而不是在现有 Anthropic 外部池逻辑里硬塞 Kiro body。

## 当前图片大小限制与 token 估算的关系

图片 token 估算不是图片大小限制。

前面修复过的图片 token 估算用于 Claude CLI / count_tokens / autocompact，目的是避免把 base64 字符串当普通文本算成几十万 token，从而导致一两张图片就触发自动压缩。

真正限制图片大小的是：

- payload guard 中的图片 source 上限。
- `/files` 上传内存 store 的单文件和总量限制。
- 远程 URL 图片下载上限。
- Kiro 上游自身请求体限制。

当前 payload guard 中可见：

```rust
const UPSTREAM_IMAGE_SOURCE_MAX_BYTES: usize = 5 * 1024 * 1024;
```

因此不能把“图片 token 默认上限 1600”理解成限制图片最大 1600 token 或限制图片大小。它只是估算口径，不是传输限制。

## 建议的配置拆分

后续不要做一个笼统的“图片直传”开关，建议拆成协议、图片处理、文件处理三个维度。

### imageMediaValidationMode

```text
strict
trustClient
off
```

- `strict`：decode 图片头，修正 media type。当前更接近这个模式。
- `trustClient`：信任下游 `media_type`，不 decode 图片字节。
- `off`：不做图片校验，只在 schema 转换失败时报错。

### fileSourceMode

```text
localMaterialize
reject
externalSticky
```

- `localMaterialize`：当前模式，`file_id` 从本地 store 取出并转 base64。
- `reject`：不支持 `file_id`，只接受 inline base64。
- `externalSticky`：上传和后续调用绑定到同一个外部池或上游，需要额外调度设计。

### externalPool.requestMode

```text
anthropicRaw
anthropicNormalized
kiroConverted
```

- `anthropicRaw`：尽量用原始 body，只做必要 model/header 调整。
- `anthropicNormalized`：当前接近该模式，先解析并标准化 payload 再转发。
- `kiroConverted`：新增 Kiro 兼容外部池模式，需要完整响应转换和 usage 适配。

### localKiro.imageMode

```text
safe
light
minimalWithLimits
```

- `safe`：完整处理，兼容性优先。
- `light`：只做 schema 转换，不 decode、不读取尺寸、不修正 media type。
- `minimalWithLimits`：不 decode，但保留请求体总量、单图 base64 长度、图片数量等最小限制。

## 推荐实现顺序

如果后续要实现，建议顺序如下：

1. 先增加配置模型，不改变默认行为。
2. 给外部池增加 `requestMode = anthropicRaw | anthropicNormalized`，先不要做 Kiro 外部池。
3. 给本地 Kiro 增加 `imageMode = safe | light`。
4. 把 `normalize_base64_image_media_types` 做成可关闭。
5. 把图片 token 估算和图片 media 校验解耦。
6. 对 `file_id` 明确配置：支持本地物化，或直接拒绝。
7. 最后再设计 `externalPool.protocol = kiro`，包含请求、响应、usage、错误、流式事件完整适配。

## 验证建议

本地凭证模式至少验证：

- inline PNG/JPEG/GIF/WEBP 图片。
- data URL 图片。
- `tool_result.content[]` 中的图片。
- 一张图、两张图、多张图下 Claude CLI autocompact 状态。
- 错误 media_type 在 `safe` 和 `light` 模式下的差异。
- 非流式和流式请求。
- `/v1`、`/cc/v1`、`/ha/v1`、`/na/v1`、dfcache 相关路径。
- `count_tokens` 和实际 `/messages` 一致性。

外部池模式至少验证：

- Anthropic 兼容外部池 raw 模式是否真的没有触发图片 decode。
- Anthropic 兼容外部池 normalized 模式是否保持当前行为。
- 外部池直连和 fallback 两种路径。
- 流式长占用、首字高延迟、上游慢响应。
- 大图片、大文本、多图片并发下 CPU 和内存变化。
- usage 整形和计费是否不受图片处理模式影响。

如果未来做 Kiro 兼容外部池，还需要额外验证：

- Kiro eventstream 到 Anthropic SSE 的事件顺序。
- 首包、message_start、content_block_start、delta、message_delta、message_stop。
- Kiro 错误到统一错误格式。
- usage 映射。
- 外部池模型映射与 Kiro model id 转换。
- prompt cache / 路径整形 / 计费策略是否仍然正确。

## 最终判断

本地凭证模式：

- 可以实现“不解码、不落盘的 Kiro schema 转换转发”。
- 不能实现完全 byte-to-byte 图片透传。
- `file_id` 场景如果继续兼容 Claude Files API，就必须在本地或外部某处保存 bytes。

Anthropic 兼容外部池：

- 可以做到最接近原始图片透传。
- 当前实现还会先标准化 payload，不是严格 raw passthrough。
- 需要新增 request mode 或调整直连进入时机。

Kiro 兼容外部池：

- 当前没有完整模式。
- 需要新增协议类型和完整请求/响应/usage/错误/流式适配。
- 不建议只改请求 body，否则协议会错配。

最稳妥的工程路线是：

```text
本地 Kiro：增加 light image mode，减少图片 decode 和本地处理。
外部 Anthropic：增加 raw request mode，尽量不碰图片。
外部 Kiro：作为独立 protocol 后续设计，不和现有外部池 Anthropic 模式混用。
```

## 2026-07-05 重构落地设计：Body / Model / Usage / Image 能力拆分

### 新需求澄清

后续实现不应把“透传”理解成一个不可扩展的死开关。目标是：

```text
以原始 body 为基础，默认不做重处理；
允许按配置挂载少量明确的处理能力；
每个能力必须声明自己需要读取或改写 body 到什么程度；
不能因为模型、usage、图片、payload guard 等旧逻辑，偷偷进入全量解析和裁剪链路。
```

因此需要同时支持：

- 完全原始转发：不解析 body，不改 body。
- 原始转发 + 模型探测：只读取顶层 `model`，用于选池、日志或计费归类，不改 body。
- 原始转发 + 模型改写：只改顶层 `model`，其他字段、图片、messages、未知字段保持原样。
- 完整整形：进入现有 Anthropic typed payload、图片处理、payload guard、usage current path policy。

### 当前实现不满足的原因

当前外部池链路存在强耦合：

- handler 入口先 `parse_messages_payload`，所以请求已经进入完整 JSON 反序列化。
- `ExternalRouteRequest` 持有 `payload: MessagesRequest`，外部池天然依赖 typed body。
- `external_pool_prepare_request` 会从 `route.payload` 重新生成 JSON、改 model/thinking、再 `serde_json::to_vec`。
- current path usage projection 依赖 `route.payload.stream`、`route.payload.model`、`request_input_tokens` 和 prompt cache profile。
- 图片处理、file source 物化、remote source 物化、media type 修正都在外部池 direct 前执行。

所以如果只在发送前把 body 换回 raw body，仍然无法降低 CPU/内存开销。真正的 raw/targeted 模式必须在 parse body 之前早分流。

### Body 读取等级

建议把每个处理能力标注为以下等级之一：

```text
None
  不读取 body 内容，只看 path、headers、body len。

TopLevelProbe
  只扫描有限范围的顶层字段，例如 model/stream，不构造完整 JSON。

TopLevelRewrite
  只改写顶层字段，例如 model。其他字段保持原样。

FullJsonValue
  解析为 serde_json::Value，可以改任意 JSON 字段，但不进入 Anthropic typed schema。

AnthropicTyped
  解析为 MessagesRequest，允许现有图片处理、payload guard、usage 估算。

KiroTyped
  转成 Kiro request，允许本地 Kiro provider 调用。
```

配置解析时必须计算 `requiredBodyReadLevel`。如果用户配置的是 raw profile，但又打开需要 `AnthropicTyped` 的能力，应拒绝配置或明确显示已升级为 normalized，不能静默升级。

### 外部池 request body 模式

建议新增外部池请求体模式：

```text
normalized
raw_passthrough
```

`normalized` 保持当前默认行为：

- parse `MessagesRequest`
- 可执行模型映射并改 body
- 可执行 thinking normalize
- 可执行图片处理
- 可执行 payload guard
- 可执行 usage current path policy
- 重新序列化发送

`raw_passthrough` 是新的轻量基础：

- 不进入 `MessagesRequest`
- 不进入图片处理
- 不进入 file_id 物化
- 不下载 remote image
- 不进入 payload guard full shaping
- 不做 token 估算
- 默认不改 body
- 可选执行顶层 model probe 或 model rewrite

### 模型处理拆分

模型处理不应该和 body 处理混为一体。建议：

```text
none
  不读 model，不改 model。

probe_only
  只轻量读取顶层 model，用于选池、日志、计费归类，不改 body。

rewrite_top_level
  轻量读取并改写顶层 model，然后转发。其他 body 内容保持原样。

full_normalized
  使用现有 model resolution / mapping / processed model 逻辑，要求 normalized body。
```

重要语义：

- `raw_passthrough + none` 是最轻模式。
- `raw_passthrough + probe_only` 仍然不改 body。
- `raw_passthrough + rewrite_top_level` 不再是 byte-for-byte，但仍是“raw base + targeted mutation”，不能进入 full typed parse。
- 如果需要 thinking normalize、schema repair、图片修正，就不应该叫 raw passthrough，而是 normalized。

### Usage 按路径整形拆分

当前 `current_path_policy` 既包含“改写响应 usage”，也包含“基于请求 token/prompt cache 的本地模拟”。这对 raw 模式不合适，需要拆成响应侧能力：

```text
passthrough
  响应完全原样，不解析 usage。

record_only
  响应原样返回，只旁路解析 usage 记录系统日志。解析失败不影响响应。

project_from_upstream_usage
  只基于上游响应 usage + path policy 做轻量整形，不依赖请求 token 估算。

current_path_policy_full
  当前完整逻辑，依赖 request_input_tokens、prompt cache、model capabilities。需要 typed request。
```

禁用优先级：

```text
path policy deny
  > pool policy deny
  > request mode capability limit
  > response usage mode allow
```

也就是说，路径设置里禁用非流式整形时，外部池即使允许也不能覆盖。

### 图片处理 profile

图片处理能力只挂到本地 Kiro 和 normalized 外部池，不挂 raw 外部池：

```text
safe
  当前兼容优先模式：file_id 物化、remote URL 物化、media type 修正、尺寸/token 估算、payload guard。

light
  轻量模式：inline base64 只做必要 schema 转换，不 decode、不读取尺寸、不修正 media type。
```

建议 light 默认策略：

- `file_id`：reject，避免内存 store 和 base64 encode。
- remote image URL：reject，避免代理下载图片。
- media type：trust client。
- token estimate：fixed image estimate。
- guard：只保留 cheap limits，例如 body bytes、图片数量、单图 source 字符串长度。

### 不同目标挂载的能力

本地 Kiro 凭证：

```text
AnthropicTyped -> KiroTyped
model: full_normalized
image: safe/light
usage: full
payload guard: configurable
```

普通外部池 normalized：

```text
AnthropicTyped
model: full_normalized
image: safe/light/off
usage: current_path_policy_full 或 project_from_upstream_usage
payload guard: configurable
```

外部池 raw proxy：

```text
Raw
model: none/probe_only/rewrite_top_level
image: off
schema: off
payload guard: cheap limits only
usage: passthrough/record_only/project_from_upstream_usage
```

未来 Kiro 官方外部上游：

```text
AnthropicTyped -> KiroTyped
response: Kiro -> Anthropic adapter
usage: Kiro usage adapter
```

该模式不应和 raw proxy 混用。

### 验收标准

必须新增测试证明：

- raw passthrough 下，外部池收到的 body 与原始 body byte-for-byte 一致。
- raw + model rewrite 下，只改顶层 model，messages/images/未知字段不被重新序列化或重排。
- raw 模式不调用 `parse_messages_payload`、file materialize、remote materialize、media type normalize、token count、payload guard full shaping。
- normalized 模式保持现有模型映射、thinking normalize、usage projection 行为。
- usage path policy 的非流式禁用开关仍然是否决开关。
- 本地 Kiro 图片 safe/light 均能处理 inline 图片；light 不 decode 图片。
- 真实 Kiro 凭证低并发请求通过。
- 真实外部池 raw proxy 请求通过，并验证响应 usage 记录/透传策略符合配置。

## 2026-07-05 实现状态

本轮已落地的配置和默认行为：

- 全局 `imageProcessing`：
  - `mode = safe | light`
  - 默认 `safe`，保持旧行为。
  - `safeMaterializeFileSources`、`safeDownloadRemoteSources`、`safeNormalizeBase64MediaTypes` 默认开启。
  - `light` 保存时会归一化为三个 safe 开关全关。
- 外部池 `requestBodyMode`：
  - `normalized`：默认，继续走现有 typed payload、图片处理、payload guard、usage projection 上下文。
  - `raw_passthrough`：解析前早分流，不进入 `parse_messages_payload`，不进入图片处理、file materialize、remote download、payload guard typed shaping。
- 外部池 raw body 的产品语义：
  - `Body 模式` 只控制是否进入 body 处理链路：`标准处理` / `Raw 透传`。
  - raw body 下是否改写顶层 `model` 是模型处理能力，不绑定在 body 下拉里；UI 在 `模型处理` 区域提供 `写回顶层 model` 开关。
  - 开启 `写回顶层 model` 后，目标值来自现有外部池模型处理配置：`modelMappingMode`、`modelMappingRules`、`modelMappingRequireMatch`、`normalizeModelVersionDots`。
  - 关闭 `写回顶层 model` 时，body 和顶层 `model` 都原样透传。
  - 低层 `rawModelMode = none | probe_only | rewrite_top_level` 仍保留用于兼容旧数据和直接 API，但后端不会强制覆盖历史配置。

本轮明确不做的部分：

- 未把本地 Kiro 凭证改成 byte-to-byte raw 透传；本地凭证仍必须做 Anthropic -> Kiro schema 转换。
- 未实现 `externalPool.protocol = kiro`。如果外部上游是 Kiro 原生协议，需要单独做请求、响应、usage、错误、流式事件适配。
- raw 外部池当前不构建 current path usage projection 上下文；如需 response-only usage project，需要后续单独拆 `usageProjectionMode`。
- light 图片模式为了避免本地下载和展开，会拒绝非 inline 的 file/remote source；inline base64/data URL 仍进入本地 Kiro schema 转换。

本轮已同步的 UI：

- `ui`
- `admin-ui`

两套 UI 均提供：

- 外部池请求体模式：标准处理 / Raw 透传。
- raw body 的顶层 model 写回开关放在现有外部池模型处理区域。
- 运行配置图片处理：Safe / Light，以及 safe 模式下三个细分开关。

本轮静态与单元测试记录：

- raw passthrough byte-for-byte 单测已覆盖。
- raw probe_only 不改 body 但能映射 model 单测已覆盖。
- raw rewrite_top_level 只改顶层 model、不改 nested model 单测已覆盖。
- raw scanner 忽略 nested model 单测已覆盖。
- safe/light 图片处理单测已覆盖。
- 全量 Rust 单元测试已通过。
- 三套前端 TypeScript/Vite 构建已通过。

## 2026-07-06 验证与修正记录

### 修正 1：外部池 list/get 漏读 body mode

实测发现：`create_external_pool` 和 `update_external_pool` 的 SQL 已经写入并返回
`request_body_mode` / `raw_model_mode`，但 `list_external_pools` 和 `get_external_pool`
的 SELECT 没有选出这两个新列。

结果是：

```text
数据库真实值：raw_passthrough / rewrite_top_level
服务读取值：normalized / none
```

这会导致 UI 和调度都认为该池还是标准处理模式，raw 入口不会命中，请求继续进入
`parse_messages_payload`、payload guard、tool_result repair 和 typed JSON 重新序列化。

已修复：

- `src/storage/postgres.rs`
  - `list_external_pools` SELECT 增加 `request_body_mode, raw_model_mode`
  - `get_external_pool` SELECT 增加 `request_body_mode, raw_model_mode`
- `src/external_pool.rs`
  - `UpdateExternalPoolRequest` 增加 `Default`，方便局部更新测试。
- 新增 PgSQL 回归测试：
  - `postgres_external_pool_list_and_get_preserve_body_modes`
  - 覆盖 create / list / get / update 后 body mode 不丢失。

### 修正 2：请求级 externalPools 配置传递

raw 入口和 normalized fallback 之前直接从 provider runtime config 读取外部池配置，
没有进入 `RequestRuntimeConfig` 的统一合并视图。为避免启动配置、PgSQL runtime config、
UI 更新后的请求级行为不一致，已将 `ExternalPoolsConfig` 纳入：

- `AppState`
- `RequestRuntimeConfig`
- `create_router_with_provider(...)`
- `main.rs` 路由初始化参数

消息请求中外部池配置统一从 `runtime_config.external_pools` 获取。

### Fake upstream 验证结果

临时环境：

```text
kiro-rs: 127.0.0.1:19082
fake upstream JSON: 127.0.0.1:19083
fake upstream SSE: 127.0.0.1:19084
PgSQL: 独立临时库
```

验证结论：

- `rawModelMode = rewrite_top_level`
  - 上游收到原始 body 的空格和字段顺序。
  - 只把顶层 `"model" : "client-model"` 改成 `"model" : "mapped-model"`。
  - nested `model` 保持不变。
  - image data URL 保持不变。
  - 没有 `trimmed orphan`，没有 payload guard 日志。
- `rawModelMode = none`
  - 上游收到 body 与下游请求 byte-for-byte 一致。
  - 顶层 model 不改写。
- `stream = true`
  - raw body 仍保留原始结构并完成顶层 model 改写。
  - SSE fake 上游返回 `message_start`、`content_block_delta` 等事件，下游可收到 SSE。
- 小压测：
  - 4 并发 20 请求：20/20 成功。
  - 8 并发 40 请求：34 成功、6 个 503，符合临时池 `maxConcurrentRequests=5` + fail-fast 行为。
  - raw 路径 payload guard 日志为 0。

### 真实外部池验证结果

临时环境中将外部池短暂切到用户提供的真实上游，测试后已恢复 fake 上游。

真实上游 models 返回 9 个模型，实际可用模型名为横杠形式，例如：

```text
claude-haiku-4-5
claude-haiku-4-5-20251001
claude-sonnet-4-5-20250929
claude-sonnet-4-6
```

验证结论：

- 使用错误模型名 `claude-haiku-4.5` 时，上游返回 `model_not_found`，系统对下游统一成临时失败错误，没有泄露内部池/调度细节。
- 使用真实模型名 `claude-haiku-4-5`：
  - 非流式 `/v1/messages` 返回 200。
  - 响应包含上游 usage。
  - 流式 `/v1/messages` 返回 200，Content-Type 为 `text/event-stream`。
  - 下游可收到 `message_start` 和 `content_block_delta`。

### 真实本地 Kiro 凭证验证结果

临时环境：

```text
kiro-rs: 127.0.0.1:19085
PgSQL: 独立临时库
externalPools: disabled
credentials: 根目录 credentials.json 首次导入
```

验证结论：

- 启动后导入 5 个本地凭据。
- 过期 access token 触发 Social Token 刷新。
- 模型能力同步成功。
- 文本非流式请求 `claude-haiku-4-5` 返回 200。
- 普通 RGB PNG inline base64：
  - `imageProcessing.mode = safe` 返回 200，Kiro 上游能识别图片。
  - `imageProcessing.mode = light` 返回 200，Kiro 上游也能识别图片。
- `light` 模式下 remote image URL：
  - 本地直接 400。
  - 错误信息明确说明 light 只接受 inline base64/data URL。

注意：1x1 透明 PNG 曾返回上游 `IMAGE_FORMAT_UNSUPPORTED`。换成普通 RGB PNG 后同一链路成功，
因此该失败不是 raw/body 重构导致，而是上游 Bedrock/Kiro 对边缘 PNG 格式不支持。测试图片应避免使用透明、
灰度/索引色、过小或非常规编码格式来代表真实截图。
