# JSON 空白压缩的逐字节语义与性能

Status: `implementation-fixed / provider-byte-capture-pass / release-size-and-burst-pass / final-candidate-rebind-pending`

Severity: P0/P1

## 问题、现象与影响

运行时配置把 `whitespaceCompression` 描述为“只处理多余空格和换行，改动最小”，但原实现先把完整 body 解析成 `serde_json::Value`，再序列化成新 JSON。该过程会重排对象键、折叠重复键、规范化数字表示和 escape。它不依赖 `bashHash...` 等泄漏指纹，通常表现为上游请求语义或签名字节异常、工具 schema 行为变化、难以复现的模型错误。

真实修复前红测输入包含重复 `z` 键、`1.0`、`1e+02` 和超出 `u64` 的整数；输出从原顺序改成 `a,z`，重复键消失，数字变成 `100.0` 与 `1.8446744073709552e+19`。因此原实现不能称为单纯空白压缩。

## 根因与源码链

[`src/http_client.rs`](../../src/http_client.rs) 的 `maybe_compress_json_whitespace` 使用 `serde_json::from_str::<Value>` 和 `serde_json::to_string`。`Value` 是语义对象，不保存原始 lexical token；项目也未启用对象顺序保留。普通 API、MCP 和相关 provider 路径都会在发送前调用该函数，所以影响面不是单一路由。

配置关闭时原实现直接返回输入，字节身份没有问题；风险只在同时启用 compression master 和 whitespace 子开关时出现。

## 复现方法

最小复现：对以下 body 开启该函数，并逐字节比较输入 token 与输出，而不是只比较解析后的 `Value`：

```json
 { "z" : 1.0 , "a" : 1e+02 , "z" : 18446744073709551616 }
```

专项矩阵每类执行至少 5 轮：对象顺序与重复键；`1.0`、指数、负零和大整数；Unicode literal 与 `\u`/`\/`/换行/反斜杠 escape；嵌套数组与未知字段；无效 JSON；关闭开关时 raw identity；1 KiB、100 KiB、1 MiB、5 MiB、50 MiB body；127/128/256/4096 层合法和畸形 JSON；5 MiB/50 MiB burst；同步任务 abort 后恢复。

provider 集成使用 raw TCP fake upstream 捕获真实请求，比较 path、content-type、Content-Length、SHA-256 和原始 bytes；矩阵覆盖 IDE/CLI、API key/profile、stream/non-stream、compression off/on，每格 5 轮，共 80 次发送。MCP 的默认 body transform 和 provider 调用点已静态确认使用同一个 lexical helper；MCP 原始 wire-byte 捕获仍由最终协议总矩阵补证，不能用 API 捕获替代宣称完成。

## 选定修复与性能方案

实现改为 lexical minifier：按 UTF-8 bytes 跟踪 JSON string 与 escape 状态，只删除字符串外 JSON 标准允许的四种空白，保留所有其他 token bytes、键顺序和重复键。修改前仍用 `IgnoredAny` 验证原 body；无效 JSON 原样返回，避免把无效 token 拼接成另一段内容。

紧凑 JSON 使用无分配 fast path：只有检测到字符串外空白才进入验证和压缩。验证不构造 `Value`；有效输入随后在原 `String` 的 `Vec<u8>` allocation 内用 read/write index 就地压缩，输出保持相同 pointer 和 capacity，不再额外申请一个 body-sized 缓冲。时间复杂度 O(n)，压缩本身额外空间 O(1)。

## 验证结果

第一组修复前专项测试 1/3 通过、2/3 按预期失败，明确捕获重复键、顺序和数字改写；第二个 allocation 红测又证明中间版本仍申请同尺寸输出缓冲。最终就地实现的 focused suite 为 5/5 通过，另有 2 个按设计 ignored 的独立性能 probe。token/escape、无效/关闭、pointer/capacity、深层合法/畸形和异常后恢复均每类至少五轮。

release binary `375f682c19462aae922d6fe0a7b9c947bb293f10e5745c30ebc7bfd2937e4bec` 的有效输入结果如下；每档五轮，allocator 统计全部为 `0 ops / 0 bytes / 0 peak live`：

| 输入 | p50 | p95/p99 | 进程 max RSS |
| --- | ---: | ---: | ---: |
| 1 KiB | 1 us | 36 us | 5,931,008 B |
| 100 KiB | 158 us | 229 us | 6,356,992 B |
| 1 MiB | 1.641 ms | 1.817 ms | 9,781,248 B |
| 5 MiB | 9.030 ms | 9.804 ms | 28,016,640 B |
| 50 MiB | 83.414 ms | 84.126 ms | 111,165,440 B |

空测试进程 max RSS 为 5,865,472 B。50 MiB probe 为了跨五轮比较而同时保留原 fixture 和每轮输入 clone，因此约 106 MiB RSS 不是 transform 额外申请 100 MiB；pointer/capacity 与 allocator 共同证明 transform 本身未再申请 body-sized buffer。

50 MiB 异常矩阵同样各五轮：invalid exact identity p95 9.822 ms，仅错误对象产生固定 40 B allocation；disabled p95 0.251 ms 且零 allocation；already-compact exact identity p95 81.780 ms 且零 allocation。5 MiB `c8 x 5` 共 40 次为 69 ms、max RSS 75,530,240 B；50 MiB `c4 x 5` 共 20 次为 491 ms、max RSS 268,681,216 B；两组随后小请求恢复 5/5。

raw provider 捕获 80/80 通过，证明 API 实际发送链没有二次 `Value` roundtrip。最终发版仍需把相同 focused gate 绑定到冻结 tag binary，并由总协议矩阵补 MCP wire capture。

## 残余风险与回滚

### 2026-07-17 共享批次复核

在 [Body / Payload identity matrix](../evidence/body-payload-identity-matrix-20260717.md) 的唯一 scoped debug+release 批次中，`http_client::tests` 为 `17/17` 通过、3 个隔离 probe 在普通发现中 ignored。新增 raw/no-op identity 用例对 1 KiB、100 KiB、1 MiB、5 MiB 的 disabled 与 already-compact 路径各执行 100 轮，逐轮验证 bytes、pointer 和 capacity；token/escape、invalid、in-place、deep/malformed、5 MiB 和 body timeout/recovery 组也全部通过。

随后显式执行 release：四档尺寸 x valid/invalid/disabled/compact x 5 轮、5 MiB `c8 x 5` burst/recovery，以及 5 轮 abort/recovery，所有 probe 退出成功。执行器截断了中段性能打印，因此本轮只声明断言合同和矩阵通过，不新增或替换精确 p50/p95/p99。批次结束删除 `2,410,696 KiB` scoped target，`removed=true`、`reservation_released=true`，路径/reservation/PID 独立复核均为零。

`IgnoredAny` 在当前 `serde_json 1.0.148` 中走迭代式 `ignore_value`，不是 Rust 递归调用；127/128/256/4096 层均已验证。它会按嵌套深度使用小型 scratch buffer，时间和空间仍是 O(n)/O(depth)。未来若需要处理 JSON5、注释或签名协议，不能放宽当前 JSON 合同后静默改写。

同步 transform 不能被 Tokio `abort` 在函数内部抢占。隔离 debug probe 中，5 MiB abort 等待 p95 141.855 ms，50 MiB p95 1.339 s，5/5 都是 CPU poll 完成后才观察到 abort；release 正常 50 MiB p95 是 84.126 ms，生产量级应以后者为准，但“不可中途取消”是明确残余语义。异常后恢复 5/5；未来若大 body 事件循环延迟超过总负载门禁，应在 provider 层把大体积同步变换迁移到有界 blocking executor，而不是破坏 lexical 合同。

回滚可关闭 compression master 或 whitespace 子开关；不得恢复 `Value` reserialization。若 lexical 实现出现问题，应回退为 raw identity，而不是做语义级 normalize。
