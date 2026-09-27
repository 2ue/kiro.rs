# P001：Kiro 体积守卫字符集加权验证计划与进度

更新时间：2026-09-27 Asia/Shanghai

## 目标

围绕生产证据 P001，验证当前 Rust `kiro.rs` 的本地 Kiro payload guard 是否把
`序列化 JSON 字节数`错误地当成了上游体积口径，并与 `../sub2api-kiro` 中已经
存在的 Kiro 字符集加权实现做逐项对照。

本轮先完成分析和对照测试，随后按用户最新要求把 Kiro weighted 逻辑迁入当前
Rust 系统；没有修改数据库、Redis、远端生产机，也没有对远端服务施加负载。

## 已知事实

- P001 有 2 条本地账号错误，均来自同一脱敏凭据编号 `#1423`、同一
  `/dfcache/onlylocal/v1/messages` 路由和同一 Sonnet 4.5 上游模型。
- 两条请求在本地 guard 后均为：
  - `originalBytes=490457`
  - `maxBytes=428032`
  - `finalBytes=407120`
  - `stillOversized=false`
  - `historyTrimPasses=1`
  - `trimmedHistoryEntries=2`
  - `finalHistoryEntries=40`
- 上游仍返回确定性 `400 CONTENT_LENGTH_EXCEEDS_THRESHOLD`。
- `../sub2api-kiro` 的实测实现使用：
  - ASCII 字符权重 `1`
  - 非 ASCII 字符权重 `8`
  - 默认加权阈值 `1,300,000`
  - 该阈值来自真实上游二分探测，不能从字节阈值直接推导。
- 当前 Rust `PayloadGuardConfig` 和 `PayloadGuardReport` 只有 `max_bytes`、
  `original_bytes`、`final_bytes` 等字节口径字段，没有 Kiro weighted metric。

## 测试计划

1. **基线确认**
   - 跑完当前 Rust payload guard focused suite，确认既有 repair、trim、tool
     pairing、image 和 serialisation 合同没有回归。
   - 跑完 `sub2api-kiro` 中字符集权重和阈值相关的 unit tests。

2. **算法对照**
   - 复核 ASCII、中文、混合文本、emoji/多字节 UTF-8、同字节数不同字符集。
   - 记录 `len(serialized_json)` 与 `ascii*1 + non_ascii*8` 的差异。
   - 验证 JSON 序列化后实际字节，而不是源码字符串或 token 数，才是应计量的
     输入。

3. **当前 Rust 行为验证**
   - 使用现有 guard 的真实 `KiroRequest` fixture，确认当前实现只按 bytes
     触发/不触发 trim。
   - 建立“字节判断”和“加权判断”相反的边界样本，明确当前代码在哪些输入上会
     过早裁剪、在哪些输入上可能放行到上游。
   - 不把临时探针误写入生产代码；需要新增代码合同时，先记录为待决策项。

4. **P001 可归因性**
   - 根据现有 usage 记录能证明什么、不能证明什么分别列出。
   - 在没有原始请求 body 的前提下，不声称已经算出了 P001 的真实 weighted
     值；只给出可计算范围和需要补采的字段。
   - 判断 `407120 bytes` 的 400 是否能由已知 weighted 规则解释，或仍需
     endpoint/model/account 维度实测。

5. **稳定方案建议**
   - 区分通用外部/Anthropic byte guard 与 Kiro-specific weighted guard。
   - 评估 `compress_then_trim`、`on_upstream_400`、`reject` 三种行为对上下文
     丢失、额度、CLI 继续执行和可观测性的影响。
   - 先提出证据支持的修改，再在本地 Rust 代码中落地 Kiro-specific weighted guard。

## 验收标准

- 逐条记录所有已执行测试、命令、结果和局限。
- 明确区分：已证实、强指向、尚未证实。
- 不泄露账号邮箱、令牌、API key、完整请求正文或原始敏感 JSON。
- 形成中文可读的结论、改进优先级、未修改事项和下一步最小补证方案。

## 已完成的对照结果

### 1. 字符集加权性质

`sub2api-kiro` 的测试和本地独立计算得到同一组性质：

| 输入 | UTF-8 bytes | Kiro weighted |
|---|---:|---:|
| `hello` | 5 | 5 |
| `中文` | 6 | 16 |
| `🙂` | 4 | 8 |
| `é` | 2 | 8 |
| ASCII 300 字节 | 300 | 300 |
| 中文 100 字（同为 300 字节） | 300 | 800 |

因此它不是“字节数乘一个固定系数”，而是逐字符计价：
`ASCII=1`，`非 ASCII=8`。JSON 结构、字段名和标点仍按 ASCII 字节各计 1。

### 2. 与当前 Rust bytes 守卫的相反判断

当前 Rust `guard_kiro_request` 在 `src/anthropic/payload_guard.rs` 中以
`serialized_body.len()` 和 `PayloadGuardConfig.max_bytes` 比较；运行态默认配置是
`450 KiB - 32 KiB = 428032 bytes`。它没有计算 weighted 值。

在同一 `428032` 字节目标下：

| 负载主体 | bytes 口径 | weighted 口径（约） | bytes 守卫 | weighted 1.3M |
|---|---:|---:|---|---|
| 全 ASCII | 428032 | 428032 | 达到边界 | 未超限 |
| 三字节中文 | 428031 | 1141416 | 达到边界 | 未超限 |
| 二字节非 ASCII | 428032 | 1712128 | 达到边界 | 超限 |
| 四字节 emoji | 428032 | 856064 | 达到边界 | 未超限 |

这已经足以证明两种口径在算法上不等价；它不等于已经证明所有 Kiro
endpoint/model 都使用同一组 1.3M 参数。

### 3. 当前 Rust 运行/性能基线

- focused payload guard suite：`88 passed / 1 ignored / 0 failed`。
- Kiro trim/repair 子集：`7 passed / 0 failed`。
- release bytes/serialization probe：`1 passed / 0 failed`，覆盖
  1 KiB、100 KiB、1 MiB、5 MiB，clean Anthropic、dirty Anthropic 和 clean
  Kiro 三种形态，每种 5 轮。
- 该 probe 证明当前 Kiro clean payload 的稳定行为是“每次真实序列化一次、按
  字节保持不变”；它没有 weighted 断言，因此不能替代字符集边界测试。

### 4. P001 当前能否直接算出 weighted

不能。P001 只保留了序列化后的：

- `finalBytes=407120`
- `historyBytes=403411`
- `currentContentBytes=3330`
- `historyEntries=40`
- 无 tool、image、reasoning 内容

在不知道 407120 字节中 ASCII 与各 UTF-8 宽度字符分布的情况下，weighted 只能给
出范围。对合法 UTF-8 字节而言，理论最低值接近 `407120`；如果大量是二字节
非 ASCII，理论值可接近 `1628480`；如果主体是三字节中文，约为 `1085653`。
所以现有记录既不能证明它一定低于 1.3M，也不能证明它一定高于 1.3M。

这也是为什么本轮不把“P001 已经证明 Rust 必须直接改成 1.3M”写成结论。已证实的
结论是：当前报告缺少与上游同口径的 weighted 指标，`stillOversized=false`
只代表本地 byte 目标满足，不代表 Kiro 上游可接受。

## 进度

| 阶段 | 状态 | 证据 |
|---|---|---|
| P001 生产证据复核 | 完成 | `tmp/prod-evidence/20260927-083000-152.53.243.159/problems/P001-local-payload-threshold-mismatch/problem.md` |
| 当前 Rust focused suite | 完成 | 迁移前 `88 passed / 1 ignored`；最终迁移后 `92 passed / 1 ignored / 0 failed` |
| `sub2api-kiro` weighted unit tests | 完成 | `go test -tags=unit ./internal/pkg/kiro -count=1 -v`，`PASS`，`3.329s` |
| 当前 Rust Kiro trim/repair focused suite | 完成 | `7 passed / 0 failed` |
| 当前 Rust 完整单测 | 完成 | 最终 scoped `cargo test --bin kiro-rs -- --nocapture`，`2187 passed / 0 failed / 6 ignored`，`128.05s` |
| Rust release build | 完成 | scoped `cargo build --release --bin kiro-rs`，`Finished release profile`，构建目标已由 wrapper 清理 |
| 前端生产构建 | 完成 | `ui npm run build` 与 `admin-ui npm run build` 均通过；`ui npm run check` 通过 |
| 构建产物门禁 | 完成 | `inventory-build-artifacts.mjs --gate`：`targets=0 reservations=0 target_processes=0 blockers=0` |
| Rust 与 weighted 对照探针 | 完成（纯算法/源码层） | 已确认当前源码只比较 `body.len()`；反向边界样本已完成 |
| P001 可归因性结论 | 完成（缺原始 body，保留不确定性） | 只能给出 weighted 范围，不能给出 P001 唯一 weighted 值 |
| Rust weighted 迁移 | 完成（本地） | Kiro guard、配置、Admin API、两套 UI、usage/log 字段已接入；未改外部 Anthropic byte guard |
| 迁移后 handler/config/external 子集 | 完成 | `16 passed / 1 ignored / 0 failed` |
| 最终中文分析文档 | 完成 | `docs/analysis/p001-kiro-payload-guard-weighted-analysis-20260927.md` |

## 约束与停止条件

- 所有 Cargo 命令必须通过 `feature/tests/run-cargo-scoped.sh`。
- 只使用指定本地验证实例 `127.0.0.1:19023`；不对远端生产服务压测。
- 真实上游大 payload 探测会消耗额度并可能触发滥用检测；除非已有明确授权和
  可回收的测试账号，否则不重复做兆字节级二分。
- 若测试结果只能证明“bytes 与 weighted 不是同一口径”，则结论停在该层，
  不虚构 P001 的精确上游阈值。

## 已落地的本地迁移

- `src/anthropic/payload_guard.rs`
  - 新增 `ASCII=1 / 非 ASCII=8` 的 Kiro weighted 计算。
  - Kiro history trim、current-fit、`still_oversized` 改按 weighted 判据收敛。
  - report/log 增加 `limitBasis/maxWeight/originalWeight/finalWeight`。
- `src/model/config.rs`
  - 新增 `payloadGuardKiroMaxWeight`，默认 `1,300,000`。
- `src/admin/service.rs`、`src/admin/types.rs`
  - Admin API 支持读取和更新该字段。
- `ui/`、`admin-ui/`
  - runtime config 类型、默认值、提交 payload 和配置控件同步。
- 外部 Anthropic/Raw 路径仍使用原有 bytes guard；新 weighted 字段不会把通用
  `payloadGuardMaxBytes` 改成全局 weight。

## 最终本地门禁

- `cargo fmt --check`：通过。
- 完整 Rust 单测：`2187 passed / 0 failed / 6 ignored`。
- Rust release build：通过，scoped wrapper 已清理临时 target。
- `ui`：`npm run check`、`npm run build` 通过。
- `admin-ui`：`npm run build` 通过。
- `git diff --check`：通过。
- `node feature/tests/inventory-build-artifacts.mjs --gate`：通过，未发现残留
  target、reservation 或 target 引用进程。

本地迁移不是“已经证明生产上游固定接受 1,300,000”的替代品。真实上游
endpoint/model/account 的低风险 fixture 仍需单独验证；在此之前，`on_too_long`
的上游错误后一次 weighted retry 仍是更保守的运行模式。
