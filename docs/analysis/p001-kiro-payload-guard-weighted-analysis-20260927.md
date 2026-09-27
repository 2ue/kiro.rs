# P001：Kiro payload 体积守卫 weighted 口径分析

更新时间：2026-09-27 Asia/Shanghai

## 范围

本文件记录 P001 的 Kiro 请求体积守卫分析和本地 Rust 迁移结果。本轮复核了生产
证据、当前 Rust 实现、`../sub2api-kiro` 的对照实现和本地测试结果，并把 Kiro
weighted guard 接入当前系统；没有修改数据库、Redis 或远端生产服务。

## 结论摘要

P001 已经证明：当前 `kiro.rs` 的本地 byte 判据不足以解释 Kiro 上游的体积 400，
并强烈指向本地与上游采用了不同的体积口径；但仅凭 P001 脱敏统计，不能把这两条
请求唯一归因成上游 `ASCII=1 / 非 ASCII=8`。当前 Rust 只按序列化 JSON 的字节数
判断；`../sub2api-kiro` 中已有的 Kiro 对照实现按字符集加权判断，即 ASCII 字符计
`1`，非 ASCII 字符计 `8`，默认安全阈值为 `1,300,000` weighted units。

这不是简单把 `maxBytes` 数字调大或调小能解决的问题。字节数与 weighted 值不是固定
比例关系：相同字节数的 ASCII、中文、二字节拉丁字符和 emoji 会得到完全不同的
weighted 值。因此稳定方案应该把 Kiro 的体积口径独立出来，至少同时记录
`originalBytes/finalBytes` 和 `originalWeight/finalWeight/maxWeight/limitBasis`，
并在确认当前 endpoint/model/account 阈值后，用 Kiro-specific weighted guard 驱动
压缩和裁剪。当前 Rust 已经完成这层本地迁移，默认
`payloadGuardKiroMaxWeight=1,300,000`；外部 Anthropic byte guard 保持不变。

P001 本身不能直接反推出那两条请求的精确 weighted 值，因为 usage 里没有保留原始
body，只保留了脱敏统计和 hash。它能证明的是：`stillOversized=false` 只代表满足本地
byte 目标，不能代表 Kiro 上游会接受。

## P001 生产现象

证据来源：

- `tmp/prod-evidence/20260927-083000-152.53.243.159/problems/P001-local-payload-threshold-mismatch/problem.md`
- `tmp/prod-evidence/20260927-083000-152.53.243.159/summary/error-ledger.md`

两条本地账号错误共同特征：

| 字段 | 值 |
| --- | ---: |
| route | `/dfcache/onlylocal/v1/messages` |
| model | Sonnet 4.5 |
| credential | 同一脱敏账号 `credential#1423` |
| upstream status | HTTP 400 `CONTENT_LENGTH_EXCEEDS_THRESHOLD` |
| inference attempts | 2 |
| external attempts | 0 |
| originalBytes | 490,457 |
| local maxBytes | 428,032 |
| finalBytes | 407,120 |
| stillOversized | `false` |
| historyTrimPasses | 1 |
| trimmedHistoryEntries | 2 |
| finalHistoryEntries | 40 |
| inputTokens | 158,038 |

payload breakdown：

| 字段 | 值 |
| --- | ---: |
| historyBytes | 403,411 |
| currentContentBytes | 3,330 |
| currentToolsBytes | 2 |
| historyImagesBytes | 40 |
| currentToolCount | 0 |
| currentToolResultCount | 0 |
| historyToolUseCount | 0 |
| historyToolResultCount | 0 |
| historyReasoningBytes | 0 |

直接判断：

- 本地 guard 已经把请求从 `490,457 bytes` 裁到 `407,120 bytes`。
- 本地报告 `stillOversized=false`，但 Kiro 上游仍返回确定性 400。
- 同一账号、同一路由、同一规模、两次尝试均失败，说明普通重试或换同类本地账号不是
  根治方向。
- 因为没有原始 body，不能复算这两条请求的唯一 weighted 值。

## 当前 Rust 实现与迁移后行为

关键位置：

- `src/anthropic/payload_guard.rs`
- `src/anthropic/handlers.rs`
- `src/model/config.rs`

迁移前行为：

- `PayloadGuardConfig` 只有 `max_bytes`。
- `PayloadGuardReport` 只有 `max_bytes/original_bytes/final_bytes/still_oversized` 等
  字节口径字段，没有 weighted 字段。
- `guard_kiro_request` 使用 `serde_json::to_string` 得到发送 body，然后用
  `serialized_body.len()` / `body.len()` 与 `config.max_bytes` 比较。
- 默认运行口径是 `payloadGuardMaxBytes=450 KiB`，再扣
  `payloadGuardSafetyMarginBytes=32 KiB`，得到有效目标 `428,032 bytes`。
- `PayloadGuardMode::OnTooLong` 下，首次请求不按体积预裁剪；遇到上游 too-long 类错误
  后，才按本地 byte 目标裁剪并重试一次。
- `stillOversized` 的语义是 `final_bytes > max_bytes`，不是
  `final_weight > limit_weight`。

迁移后行为：

- Kiro local path 新增 `payloadGuardKiroMaxWeight`，默认 `1,300,000`。
- Kiro guard 计算真实序列化 JSON 的 weighted 值：ASCII 字符计 1，非 ASCII 字符计 8。
- Kiro 历史裁剪、当前内容兜底收缩和 `stillOversized` 都按 weighted 值与
  `maxWeight` 比较。
- `PayloadGuardReport`/日志带 `limitBasis=kiroWeighted`、原始/最终 bytes 和
  原始/最终 weight，便于继续验证生产边界。
- 外部 Anthropic/Raw guard 仍按 bytes，不把通用 `payloadGuardMaxBytes` 全局改成 weight。

这修复了 P001 暴露的“本地只看 bytes、上游按字符集加权”的判据缺口；但不等于已经
在当前生产 endpoint/model/account 上重新完成真实上游阈值探测。

## `sub2api-kiro` 对照实现

关键位置：

- `../sub2api-kiro/backend/internal/pkg/kiro/payload_guard.go`
- `../sub2api-kiro/backend/internal/pkg/kiro/payload_guard_test.go`
- `../sub2api-kiro/docs/kiro-research/test-results.md`

对照实现的核心规则：

```text
ASCII character     = 1
non-ASCII character = 8
default limit       = 1,300,000
```

它逐 UTF-8 字符扫描真实 payload bytes：

```go
if c < utf8.RuneSelf {
    weight++
} else {
    weight += 8
}
```

该阈值来自真实上游探测记录：

| 场景 | weighted | 结果 |
| --- | ---: | --- |
| 中文 165,000 字 | 1,320,000 | HTTP 200 |
| 中文 170,000 字 | 1,360,000 | HTTP 400 |
| ASCII 1,280,000 字 | 1,280,000 | HTTP 200 |
| ASCII 1,361,920 字 | 1,361,920 | HTTP 400 |
| mixed `8*80,000 + 600,000` | 1,240,000 | HTTP 200 |
| mixed `8*100,000 + 600,000` | 1,400,000 | HTTP 400 |

因此 `sub2api-kiro` 选择 `1,300,000` 作为带余量的默认值。重要的是，这不是官方文档
给出的绝对协议合同，而是黑盒实测模型；所以迁回 Rust 时应该保留可配置能力和观测字段。

## 本轮测试

### 1. `sub2api-kiro` Kiro 包测试

命令：

```bash
cd ../sub2api-kiro/backend
go test -tags=unit ./internal/pkg/kiro -count=1 -v
```

迁移前基线结果：

```text
PASS
ok github.com/Wei-Shaw/sub2api/internal/pkg/kiro 3.329s
```

覆盖重点：

- weighted 字符计价：ASCII 计 1，非 ASCII 计 8。
- 同字节数、不同字符集得到不同 weighted。
- 默认阈值落在实测区间的安全侧。
- 未超限时 payload 完全不变。
- 压缩优先，压缩无效后才裁剪历史。
- 裁剪不切断 tool use / tool result 配对。
- 找不到干净切点时软失败，不强裁。
- `compress_then_trim`、`on_upstream_400`、`reject` 三种行为。
- `DeferredToUpstream` 与 `StillOversized` 语义分离。
- 压缩阶段幂等，避免上游 400 后重试路径重复截断。

### 2. 当前 Rust payload guard focused suite

命令：

```bash
feature/tests/run-cargo-scoped.sh p001-payload-guard-current-focused -- \
  cargo test --bin kiro-rs payload_guard -- --nocapture
```

结果：

```text
test result: ok. 88 passed; 0 failed; 1 ignored; 0 measured; 2100 filtered out; finished in 58.87s
```

覆盖重点：

- 当前 Rust 的 byte-based Kiro/Anthropic guard 合同仍然通过。
- Kiro history trim、tool pair 原子裁剪、orphan 修复、image drop、current payload shaping
  等既有行为没有在本轮测试中失败。
- 这组测试没有 weighted 断言，因此不能证明 P001 已经修复；它证明的是当前 byte-based
  实现稳定地按既有合同运行。

迁移后同一 focused suite：

```text
test result: ok. 92 passed; 0 failed; 1 ignored; 0 measured; 2100 filtered out; finished in 71.40s
```

新增通过的覆盖包括字符集计价、bytes/weighted 反向边界和 weighted history trim。

### 3. Rust release payload size probe

命令：

```bash
feature/tests/run-cargo-scoped.sh p001-byte-probe-release -- \
  cargo test --release --bin kiro-rs \
  anthropic::payload_guard::tests::payload_guard_release_size_matrix_probe \
  -- --ignored --nocapture
```

结果：

```text
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2190 filtered out; finished in 0.41s
```

该 probe 覆盖 `1 KiB`、`100 KiB`、`1 MiB`、`5 MiB`，每个场景 5 轮，包含：

- clean Anthropic：零拷贝，`serializations=[0,0,0,0,0]`。
- dirty Anthropic：每轮一次序列化。
- clean Kiro：每轮一次真实序列化。

关键输出：

| mode | input bytes | p50 | p99 | serializations |
| --- | ---: | ---: | ---: | --- |
| clean_kiro | 1,202 | 13 us | 123 us | 1/round |
| clean_kiro | 102,578 | 798 us | 802 us | 1/round |
| clean_kiro | 1,048,754 | 3,615 us | 3,682 us | 1/round |
| clean_kiro | 5,243,058 | 18,931 us | 19,143 us | 1/round |

解释：

- 当前 Rust 的 Kiro clean path 在 release 下性能稳定。
- 它仍然只证明 byte 序列化路径稳定，不证明 weighted 判据存在。

### 4. 字符集边界独立计算

命令使用 Node.js 逐字符计算 `ASCII=1 / non-ASCII=8`，结果如下：

| 输入 | UTF-8 bytes | chars | weighted |
| --- | ---: | ---: | ---: |
| `hello` | 5 | 5 | 5 |
| `中文` | 6 | 2 | 16 |
| `🙂` | 4 | 1 | 8 |
| `é` | 2 | 1 | 8 |
| ASCII 300 字节 | 300 | 300 | 300 |
| 中文 100 字，同为 300 字节 | 300 | 100 | 800 |
| ASCII 428,032 字节 | 428,032 | 428,032 | 428,032 |
| 中文 428,031 字节 | 428,031 | 142,677 | 1,141,416 |
| `é` 428,032 字节 | 428,032 | 214,016 | 1,712,128 |
| emoji 428,032 字节 | 428,032 | 107,008 | 856,064 |

由此可见：当前 Rust 的 `428,032 bytes` 目标与 `1,300,000 weighted` 上游模型不是同一
判断。

### 5. 迁移后的配置/UI/handler 验证

命令：

```bash
feature/tests/run-cargo-scoped.sh p001-weighted-handler-tests-2 -- \
  cargo test --bin kiro-rs payload_guard_ -- --nocapture
```

结果：

```text
test result: ok. 16 passed; 0 failed; 1 ignored
```

另有前端检查：

- `ui`: `npm run check` 通过。
- `admin-ui`: `npm run build` 通过。

这些结果证明新字段能够从 Rust 配置传播到 guard、Admin API 和 UI；不代表真实生产
上游已经重新测量。

## P001 能解释到什么程度

P001 的 `finalBytes=407,120` 只能给出范围：

- 如果几乎全是 ASCII，weighted 接近 `407,120`。
- 如果主体是三字节中文，weighted 约为 `1,085,653`。
- 如果大量是二字节非 ASCII，weighted 可接近 `1,628,480`。
- 如果大量是 emoji，weighted 约为 `814,240`。

所以 P001 有两层结论：

1. 已证实：当前 byte guard 的报告字段不足以解释 Kiro 上游体积 400；`stillOversized=false`
   不能作为“上游可接受”的证据。
2. 未证实：这两条具体请求一定是因为 `ASCII=1/non-ASCII=8/1.3M` weighted 超限而失败。
   仍需原始 body 或新增 weighted 指标才能定性。

还不能排除的因素：

- 不同 endpoint/model/account 档位使用不同阈值。
- Kiro 上游阈值在 2026-09-14 之后变化。
- 上游还叠加了字段结构、token、内部 envelope 或账号侧策略。
- P001 的 158k input tokens 可能触发了与 payload weighted 并列的限制。

## 设计判断

### 不建议的做法

1. 不建议把全局 `payloadGuardMaxBytes` 直接改成 `1,300,000`。
   这个字段名字和语义都是 bytes；直接替换会污染外部 Anthropic、external pool 和日志语义。

2. 不建议只把 `428,032 bytes` 调成另一个 byte 阈值。
   字符集差异会让任意固定 byte 阈值两头失真：某些请求被过早裁剪，某些请求仍会被放到上游后 400。

3. 不建议把超限一律 `reject` 作为默认。
   这会让本来可以通过压缩/裁剪恢复的请求直接失败，并且对交互式 CLI 体验最差。

4. 不建议在没有验证阈值前把 `compress_then_trim` 作为所有 Kiro 请求的强预检默认。
   预检的目的应该是避免确定性失败和一次无意义上游请求，不应被笼统包装成“节省积分”。
   如果阈值模型不准，预检会静默丢上下文。

### 已落地与仍需验证

1. 增加 Kiro-specific size basis。已落地。

   Kiro local route 使用 weighted basis；外部 Anthropic 和 Raw/external route 继续使用现有 bytes
   basis，除非另有上游证据。

2. 增加 weighted 观测字段。已落地。

   `PayloadGuardReport` 或 Kiro 专用扩展至少记录：

   - `limitBasis`: `bytes` / `kiroWeighted`
   - `originalBytes`
   - `finalBytes`
   - `originalWeight`
   - `finalWeight`
   - `maxWeight`
   - `stillOversized`
   - `historyTrimPasses`
   - `guardBehavior`（当前由 `payloadGuardMode` 和日志/report 字段共同表达）

3. 先观测，再收敛默认行为。当前仍保留 `on_too_long`（对应对照实现的
   `on_upstream_400`）错误后一次 retry 语义；
   新增 weighted 判据会用于 retry shaping，但尚未把所有请求改成强预检。

   在未确认当前 Rust 所用 Kiro endpoint/model/account 的真实阈值前，保留
   `on_too_long` 是更稳妥的默认：首次不过度预裁剪，遇到上游体积 400 后再用 weighted
   shrink 重试一次。

4. 阈值确认后，对确定超限请求启用 `compress_then_trim`。本地 guard 已具备
   weighted 驱动的压缩/裁剪能力，默认行为是否切到强预检仍需真实 fixture 证据。

   一旦用固定 fixture 验证当前上游仍符合 `ASCII=1/non-ASCII=8` 且安全阈值可用，
   对明显超过阈值的 Kiro 请求应先压缩、后裁剪，再发送上游。这样能避免 P001 这种
   “先失败一次，再按错误口径裁剪一次，仍失败”的路径。

5. 裁剪策略优先级应保留“压缩优先”。

   推荐顺序：

   ```text
   历史 tool result 压缩
   历史 thinking / reasoning 剥离
   tool definition 描述压缩
   历史图片丢弃并留占位
   安全切点历史裁剪
   ```

   只有最后一步真正丢整轮上下文；前几步是降低重复和冗余。

6. 裁剪必须保护 Kiro 协议结构。

   不能切断 tool use / tool result 配对；找不到安全切点时宁可软失败并让上游裁决，不能制造另一个
   `request_body_invalid`。

7. 裁剪后仍失败时，不应无界重试。

   对同一请求体积超限，最多一次 weighted shrink retry。若仍被上游拒绝，应返回明确、可诊断的
   API 层错误，告知需要缩短上下文或减少当前输入。这里不建议伪造成 HTTP 200 assistant
   成功响应，因为 payload 超限不是模型内部 tool execution failure；伪成功会污染对话状态，
   也会让调用方误以为模型已经处理了请求。

## 后续最小补证方案

1. 用迁移后的 weighted 报告继续观察：`finalBytes <= maxBytes` 但
   `finalWeight > maxWeight` 的请求是否与 Kiro 400 高度重合。

2. Rust 单元测试已补齐并通过。

   已覆盖：

   - `hello -> 5`
   - `中文 -> 16`
   - `é -> 8`
   - `🙂 -> 8`
   - 同 300 bytes 下中文 weighted 大于 ASCII
   - bytes 未超但 weighted 超限
   - bytes 超限但 weighted 未超限

   对照实现中的 `DeferredToUpstream` 当前没有作为独立 Rust report 字段复制；
   当前 `PayloadGuardMode::OnTooLong` 通过“首次配置关闭 size guard、上游体积错误后
   构造 weighted retry config”表达，不能把它误写成已经有同名字段。

3. 用小规模真实 fixture 验证当前 Kiro 上游阈值。

   不做生产压测，不做大并发。只在本地指定实例和可控测试账号上，用 3 到 6 个固定 payload 点验证：

   - ASCII 低于/高于 1.3M
   - 中文低于/高于 1.3M
   - 混合低于/高于 1.3M

   记录 HTTP status、Kiro error code、request id、weighted、bytes、model、endpoint、账号类型。

4. 验证 weighted shrink retry。

   fake upstream 先模拟 `CONTENT_LENGTH_EXCEEDS_THRESHOLD`，确认只重试一次，第二次使用 weighted
   shrink 后不再重复裁剪；再用真实上游做一条低风险确认。

5. 再决定默认行为。

   若 fixture 命中率稳定，Kiro local route 可切到 weighted `compress_then_trim` 或
   weighted `on_upstream_400 + retry`。若不同账号/模型阈值不一致，则需要按模型/账号档位配置
   `maxWeight`，不能全局硬编码。

## 本轮修改与未修改

已修改：

- `src/anthropic/payload_guard.rs`
- `src/anthropic/handlers.rs`
- `src/anthropic/middleware.rs`
- `src/anthropic/router.rs`
- `src/model/config.rs`
- `src/admin/service.rs`
- `src/admin/types.rs`
- `src/anthropic/body_capabilities.rs`
- `src/anthropic/payload_guard_runtime.rs`
- `src/anthropic/handlers/tests.rs`
- `src/external_pool/tests.rs`
- `ui/` 和 `admin-ui/` runtime config 类型、默认值、提交和控件

未修改：

- 没有修改数据库、Redis、远端生产机或远端运行配置。
- 没有做真实上游兆级 payload 二分探测。
- 没有把外部 Anthropic/Raw 路径改成 Kiro weighted。

## 本轮产出

- 计划与进度文档：
  `docs/analysis/p001-payload-guard-weighted-validation-20260927.md`
- 最终分析文档：
  `docs/analysis/p001-kiro-payload-guard-weighted-analysis-20260927.md`

这次的结论是：“P001 的根因方向已经收敛到 Kiro-specific 体积口径缺失，weighted
方案已迁入当前 Rust，并有本地测试覆盖；剩余工作是用当前真实 endpoint/model/account
做低风险 fixture 验证阈值，再决定是否把默认行为从错误后 retry 收紧为强预检。”
