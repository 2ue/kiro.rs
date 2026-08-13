# Kiro 官方图片 5MB 限制与多图方案调查

日期：2026-07-02

## 结论

可以用“多张图片”解决单张图片超过 5MB 的问题，但不能把一段 base64 文本硬切成几段。

正确做法是：把原始大图先处理成多张独立、合法、可打开的图片，每张图片重新编码后都要小于官方限制，然后按多图请求发给 Kiro 官方上游。

真实官方测试已经验证：

- 单张小图可以成功。
- 单张超过 5MB 的图片会被官方拒绝。
- 两张图片各自小于 5MB，但合计超过 5MB，官方可以成功处理。

所以官方限制更像是“每一张图片自己的 `source.bytes` 不能超过 5MB”，不是“整个请求里所有图片加起来不能超过 5MB”。

## 本地代码事实

当前项目已经有本地保护，位置在：

- `src/anthropic/payload_guard.rs:31`
- `src/anthropic/payload_guard.rs:1419`
- `src/anthropic/payload_guard.rs:1460`

关键代码事实：

```rust
const UPSTREAM_IMAGE_SOURCE_MAX_BYTES: usize = 5 * 1024 * 1024;
```

当前判断图片大小时，看的是 Kiro 图片里的 `source.bytes` 长度：

```rust
fn kiro_image_source_bytes(image: &crate::kiro::model::requests::conversation::KiroImage) -> usize {
    image
        .source
        .bytes
        .as_ref()
        .map(|bytes| bytes.len())
        .unwrap_or_else(|| json_len(image))
}
```

这里的 `source.bytes` 是 base64 字符串，不是图片文件解码后的二进制大小。

真实官方返回也证明官方按这个字段的字符串长度做限制。也就是说，一张二进制 3.8MB 左右的图片，base64 后可能已经接近或超过 5MB。

## 真实官方测试记录

测试结果文件：

- `tmp/official-image-test/official-image-results.json`

测试时为了看官方真实行为，临时关闭了本地 payload guard 和 payload shaping，避免本地逻辑提前拦截。测试完成后已经恢复原配置。

测试前运行时配置：

```json
{
  "payloadGuardEnabled": true,
  "payloadGuardMaxBytes": 460800,
  "payloadShapingEnabled": true
}
```

测试期间运行时配置：

```json
{
  "payloadGuardEnabled": false,
  "payloadGuardMaxBytes": 0,
  "payloadShapingEnabled": false
}
```

测试后已恢复为：

```json
{
  "payloadGuardEnabled": true,
  "payloadGuardMaxBytes": 460800,
  "payloadShapingEnabled": true
}
```

### 测试 1：单张小图

图片：

- `small.png`
- 文件大小：12,420 bytes
- base64 长度：16,560 字符

结果：

- HTTP 状态：200
- 官方正常返回
- 说明普通图片路径没有问题

### 测试 2：单张超大图

图片：

- `oversized.png`
- 文件大小：5,532,303 bytes
- base64 长度：7,376,404 字符

结果：

- HTTP 状态：400
- 官方拒绝
- 官方错误原因：图片超过 5MB 限制，`source.bytes` 大于 `5,242,880`

这说明官方不是按本地图片文件大小限制，而是按请求里图片 base64 字符串长度限制。

### 测试 3：两张图片，每张都小于 5MB

图片：

- `tile_a.png`
  - 文件大小：3,380,878 bytes
  - base64 长度：4,507,840 字符
- `tile_b.png`
  - 文件大小：3,380,878 bytes
  - base64 长度：4,507,840 字符

合计 base64 长度：

- 9,015,680 字符

结果：

- HTTP 状态：200
- 官方正常返回
- 官方能识别为两张图

这证明多图方案可行：只要每张图片自己的 `source.bytes` 小于官方限制，总体超过 5MB 也不一定会被拒绝。

## 不能怎么做

不能直接把一张大图的 base64 字符串切成几段，然后伪装成多张图片。

原因很简单：切开的每一段都不是完整图片。官方收到后不是“拼回原图”，而是按多张图片分别解析。每一张都必须是能独立打开的合法图片。

所以要做的是图片级处理：

- 解码原图。
- 按宽高裁成多块，或者先压缩、缩放。
- 每块重新编码成 PNG/JPEG/WebP 等格式。
- 每块再转 base64。
- 每块都要小于官方限制。

## 推荐实现方案

建议不要把这个逻辑直接塞进现有 reject/drop 分支里，而是在 payload shaping 里增加一个明确的图片处理策略。

默认行为必须保持不变，避免线上突然改变图片处理方式。

推荐流程：

1. 先检查当前请求里的图片是否超过官方限制。
2. 如果没有超过，什么都不做。
3. 如果超过，先尝试压缩或缩放，让单张图直接降到限制内。
4. 如果压缩后仍然超过，并且配置允许拆图，再把图片切成多张 tile。
5. 每张 tile 都要重新编码，并检查 base64 长度。
6. 在用户消息里追加一句说明，告诉上游这些图片来自同一张原图，顺序是从左到右、从上到下。

示例说明文本可以是：

```text
The original image was split into 4 tiles, ordered left-to-right and top-to-bottom.
```

中文说明不建议发给上游，因为请求内容本身可能是英文任务；这里最好用稳定的英文说明。

## 参数建议

这些参数是为了控制风险，不是为了堆功能。

```json
{
  "payloadShaping": {
    "oversizedImageHandling": "compressThenSplit",
    "oversizedImageTileMaxBase64Bytes": 4700000,
    "oversizedImageTileMaxCount": 6,
    "oversizedImageSplitCurrentOnly": true,
    "oversizedImagePreferCompression": true,
    "oversizedImageMaxDecodedBytes": 20000000,
    "oversizedImageMaxOutputBase64Bytes": 24000000
  }
}
```

参数含义：

- `oversizedImageHandling`：超大图片怎么处理。建议新增 `compressThenSplit`，意思是先压缩，压不下来再拆图。
- `oversizedImageTileMaxBase64Bytes`：每张拆出来的小图最多允许多少 base64 字符。建议小于官方 5MB，留一点空间，不要卡着上限发。
- `oversizedImageTileMaxCount`：最多拆几张图。防止一张极大的图被拆成几十张，拖慢请求或撑爆内存。
- `oversizedImageSplitCurrentOnly`：默认只处理当前这轮用户新发的图片。历史消息里的超大图不建议重新拆，因为历史内容可能越来越多，会让请求体持续变大。
- `oversizedImagePreferCompression`：优先压缩或缩放。能一张图解决时，不要拆成多图。
- `oversizedImageMaxDecodedBytes`：解码后的图片最大允许占用。防止超高分辨率图片在内存里展开后变得非常大。
- `oversizedImageMaxOutputBase64Bytes`：拆图后所有输出图片的 base64 总量上限。防止单张图虽然拆成功，但总请求变得过大。

建议默认值：

- 默认不启用拆图，保持现网行为。
- 如果启用拆图，每张 tile 的 base64 上限建议先用 `4,700,000`，不要贴着 `5,242,880`。
- 最大 tile 数建议先用 `4` 或 `6`，不要无限拆。

## 内存和性能风险

这个功能不能只看“能不能发给官方”，还要看本地服务会不会被拖慢。

主要风险：

- 图片解码后会比文件本身大很多。例如一张 8000x8000 的 RGBA 图片，展开后大约 256MB。
- 拆图会产生多份图片数据，短时间内会同时占用原图、解码图、tile 图、base64 字符串。
- 如果历史消息里的大图也反复拆，会让每一轮请求越来越重。
- 如果没有总量限制，用户可以用多张大图把请求体撑得很大。

所以实现时必须加硬限制：

- 限制原始 base64 长度。
- 限制解码后像素数量或内存大小。
- 限制 tile 数量。
- 限制每张 tile 的 base64 长度。
- 限制拆图后所有图片的总 base64 长度。
- 默认只处理当前用户消息，不处理历史图片。

## 后续测试要求

如果后面实现这个功能，测试不能只跑单元测试。

至少要覆盖：

- 配置关闭时，现有行为不变。
- 单张超大图在 `reject` 模式下仍然拒绝。
- 单张超大图在 `dropWithPlaceholder` 模式下仍然按旧逻辑处理。
- 开启 `compressThenSplit` 后，超大当前图片会被变成多张小图。
- 每张小图的 base64 长度都小于配置上限。
- tile 数量超过上限时要停止并返回清楚错误，不能继续硬处理。
- 历史超大图片默认不拆，避免请求越来越大。
- 错误图片、损坏图片、非图片 base64 都要安全失败。
- 大分辨率图片不能造成内存暴涨。

真实官方回归测试至少要做：

- 单张小图成功。
- 单张超大图在关闭拆图时保持当前失败或占位行为。
- 单张超大图在开启拆图时成功。
- 多张原本就小于 5MB 的图片保持成功。

## 最终判断

多图方案是可行的，而且有真实官方返回支撑。

但是它不是一个简单的字符串切分功能，而是一个图片转换功能。实现时要把默认行为保持不变，并把拆图放到显式配置下，同时加上严格的数量、大小和内存上限。这样才能既解决 5MB 单图限制，又不把本地服务拖慢或拖垮。
