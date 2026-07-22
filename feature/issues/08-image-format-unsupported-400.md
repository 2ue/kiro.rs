# 图片无法处理触发上游 400 IMAGE_FORMAT_UNSUPPORTED

Status: `structure-validation-and-400-classification-focused-pass / real-cli-image-gate-pending`

Severity: P1

- 状态：根因已定位并复现；2026-07-13 已实现轻量结构校验，待真实服务回归
- 严重级别：低 —— 生产近 12 小时仅 1 条，但同类（坏图/伪图）随客户端输入可复发
- 影响端点：全部 `/v1`、`/cc/v1`、`/ha/v1`、`/na/v1`、`/dfcache/*`（共用请求处理逻辑）
- 分类来源：`tmp/analysis-usage-llm-errors` root-cause `08-local_bad_request_unsupported_image_format`

## 现象

请求携带图片时，上游 Bedrock/Kiro 返回：

```
400 Bad Request
{"message":"Bedrock error message: The model returned the following errors: Could not process image","reason":"IMAGE_FORMAT_UNSUPPORTED"}
```

代理对外返回 HTTP 400 `invalid_request_error`（`The request body is invalid. Simplify the message, tools, tool results, files, or images and retry. ...`）。usage 记录 `errorSource: local_account`、`errorType: api_error`、`errorStatusCode: 400`，`routeSubtype: local_error_no_fallback`（400 不换号重试）。

## 复现验证结论（重要，修正早期误判）

用本地 sonnet 账号（模型 `claude-sonnet-4-20250514`）实测多种图片：

| 图片 | 内容 | 结果 |
|---|---|---|
| 合法 PNG 64×64（蓝色实心） | 正常图 | 200 ✅，模型正确回答 "Blue" |
| 合法 PNG 512×512 | 正常图 | 200 ✅，模型正确回答 "Blue" |
| 合法 PNG 1×1 | 极小图 | 200，但模型称"没看到图"（被上游静默忽略，非拒绝） |
| PNG 字节但 `media_type` 声明为 webp | magic 与声明不符但字节是真图 | 200 ✅（代理按字节识别修正） |
| **坏 PNG**（PNG 头 + 损坏内容） | 头合法、内容不可解码 | **400 IMAGE_FORMAT_UNSUPPORTED** ❌ |
| **垃圾字节声明为 PNG**（magic 不符） | 根本不是图片 | **400 IMAGE_FORMAT_UNSUPPORTED** ❌ |

**结论**：
- 正常尺寸的合法图片**不会**被拒（早期"1×1 被拒"的判断错误 —— 1×1 是被静默忽略，且那次命中的是不同账号）。
- 真正触发 `IMAGE_FORMAT_UNSUPPORTED` 的是**坏图 / 伪图**：字节无法被上游解码为有效图像。
- 上游做的是**完整解码校验**，不只是 magic-byte 检查。

## 根因

代理侧图片校验在 `src/anthropic/converter/content.rs:437` `image_format_from_base64_or_media_type`：

- 仅检查 **magic bytes** 落在 png/jpeg/gif/webp（`infer_image_format_from_bytes`，`content.rs:454`）。
- 若 magic 可识别 → 放行识别结果（能拦"声明 png 实为其它"）。
- 若 magic **不可识别** 但 `media_type` 声明合法 → **回退放行 declared**（`content.rs:451`）。

盲区：
1. **坏图**：magic 头合法（如 `\x89PNG...`）但后续字节损坏 —— magic 检查通过、放行，上游解码失败 → 400。
2. **伪图**：magic 不符但声明合法 —— 回退放行 declared，上游解码失败 → 400。

即代理只做"头部识别"，不做"完整解码验证"，无法拦下头合法/声明合法但实际不可解码的图。

## 复现 case

前置：本地服务 `127.0.0.1:9022`，API Key `sk-kiro-rs-local-debug`，模型 `claude-sonnet-4-20250514`。

### Case 1：合法图片，正常通过（对照）

```bash
# 生成合法 PNG
python3 - <<'PY'
import zlib,struct,base64
def png(w,h):
    def chunk(t,d): return struct.pack('>I',len(d))+t+d+struct.pack('>I',zlib.crc32(t+d)&0xffffffff)
    raw=b''.join(b'\x00'+b'\x00\x00\xff'*w for _ in range(h))
    return b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',w,h,8,2,0,0,0))+chunk(b'IDAT',zlib.compress(raw))+chunk(b'IEND',b'')
open('/tmp/ok.b64','w').write(base64.b64encode(png(64,64)).decode())
PY
B64=$(cat /tmp/ok.b64)
curl -sS -X POST http://127.0.0.1:9022/v1/messages \
  -H 'content-type: application/json' -H 'x-api-key: sk-kiro-rs-local-debug' \
  -H 'anthropic-version: 2023-06-01' \
  -d "{\"model\":\"claude-sonnet-4-20250514\",\"max_tokens\":32,\"messages\":[{\"role\":\"user\",\"content\":[{\"type\":\"image\",\"source\":{\"type\":\"base64\",\"media_type\":\"image/png\",\"data\":\"$B64\"}},{\"type\":\"text\",\"text\":\"one word: color?\"}]}]}"
# → HTTP 200
```

### Case 2：坏图 / 伪图，触发 400（复现 bug）

```bash
# 垃圾字节声明为 png（magic 不符）
B64=$(python3 -c "import base64;print(base64.b64encode(b'not-an-image-at-all'*8).decode())")
curl -sS -X POST http://127.0.0.1:9022/v1/messages \
  -H 'content-type: application/json' -H 'x-api-key: sk-kiro-rs-local-debug' \
  -H 'anthropic-version: 2023-06-01' \
  -d "{\"model\":\"claude-sonnet-4-20250514\",\"max_tokens\":32,\"messages\":[{\"role\":\"user\",\"content\":[{\"type\":\"image\",\"source\":{\"type\":\"base64\",\"media_type\":\"image/png\",\"data\":\"$B64\"}},{\"type\":\"text\",\"text\":\"color?\"}]}]}"
# → HTTP 400，上游日志 reason=IMAGE_FORMAT_UNSUPPORTED
```

服务端日志可见：`Bedrock error message: ... Could not process image","reason":"IMAGE_FORMAT_UNSUPPORTED"`。

## 性质判定：上游为主，程序可部分规避

- **正常图被拒**：不存在（已复现证伪）。
- **坏图/伪图被拒**：属**客户端输入错误**，上游正确拒绝；但代理当前放行、白白消耗一次上游往返并占用账号配额。

已采用轻量结构校验而不是完整解码：

1. base64 必须能成功解码；不能解码时本地拒绝。
2. 字节必须能通过 magic bytes 识别为 PNG/JPEG/GIF/WebP；不再因为 `media_type` 声明合法就回退放行伪图。
3. 对识别出的格式做最低结构校验：
   - PNG：chunk 结构有效，首个 chunk 是 `IHDR`，且存在结尾 `IEND`。
   - JPEG：有 SOI 与 EOI。
   - GIF：有合法 header 与 trailer。
   - WebP：有 RIFF/WEBP 与 VP8/VP8L/VP8X chunk 标记。

该方案不引入 `image` crate，不做完整像素解码，CPU 成本是当前内联图片字节的线性扫描。它能拦现网分析中的伪图/截断坏图；极少数语义层损坏但结构仍完整的图片仍可能由上游拒绝，这类继续依赖官方上游错误 message 透出。

约束不应放宽：合法图片本就该放行，无需改动。

## 修复方案（已实施，低优先级）

1. `content.rs` 图片转换阶段拒绝无效 base64、伪图、截断 PNG/JPEG/GIF/WebP。
2. 保持合法图片路径不变：media_type 与字节不符但字节是真图时，仍按字节识别结果修正后放行。
3. 不做完整解码，不引入重依赖；后续如生产仍出现结构完整但上游无法解码的坏图，再评估完整解码或更细结构校验。

## 回归清单

- [x] 单测：合法 base64 图片、data URL 图片、工具结果图片继续转换。
- [x] 单测：media_type 与字节不符但字节是真图 → 按字节识别结果修正。
- [x] 单测：垃圾字节声明 png → 本地拒绝。
- [x] 单测：截断 PNG → 本地拒绝。
- [ ] 真实服务：合法 64×64 / 512×512 PNG 返回 200。
- [ ] 真实服务：垃圾字节声明 png / 坏 PNG 本地提前返回明确错误，不再打到上游。
- [x] 通用 `REQUEST_BODY_INVALID` + `Image data cannot be empty` 不再分类为 `tool_use_format_bad_request`；启用 prompt-logic retry 时，1/20/60 账号池各 5 轮均恰好 1 个 provider hit。真实 CLI 的本地 0-hit/上游 1-hit 分层仍待 D05。

## 关联

- 工具/schema 边界值三类程序缺陷：`feature/issues/empty-tool-description-400-invalid-tool-use-format.md`、`feature/issues/tool-property-key-invalid-400-tool-schema-invalid.md`。
- 生产证据：`tmp/analysis-usage-llm-errors/root-causes/08-local_bad_request_unsupported_image_format/`。
- 代表 requestId：`req_011n5v5Uo5e7zqJhBRuR2cDQ`（生产，账号 #170）。
- 400 分类动态证据：`feature/evidence/provider-400-retry-classification-20260716.md`。

## 残余风险与回滚

轻量结构校验不能证明所有语义层图像都可被上游完整解码，真实 PNG/JPEG/WebP/GIF、data URL、URL、空图和 CLI image 仍需各 5 轮。回滚不得恢复声明 media type 即放行垃圾字节、确定性坏图跨账号 retry 或原始上游错误泄漏；若某个合法格式被误拒，应收窄该格式的结构规则并增加真实 fixture，而不是关闭全部图片保护。
