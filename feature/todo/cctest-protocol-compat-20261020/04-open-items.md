# 04 待决问题与下一步

## 待用户确认

| # | 问题 | 建议 |
|---|---|---|
| Q1 | WebSearch 开关的默认值和作用范围：全局默认开还是关？是否支持按路由覆盖？ | 全局默认关，按路由开启；先在测试路由上验证 cctest |
| Q2 | 结构化输出和用户自带 tools 同时出现时怎么办 | 内部工具和用户工具都提供给模型，`tool_choice` 用 auto；如果模型调了内部工具，就还原成 JSON 文本 |
| Q3 | `web_search_requests` 是否加回 usage | 先查 `ba720d1` 为什么删掉（可能和 usage 记录或计费有关）。如果会影响 usage 记录，就只在响应 JSON 里加，不进入记录链路 |
| Q4 | WebSearch 多出来的模型调用产生的 token，是否计入返回的 usage | 官方口径是累加。但这和“不改 usage 整形逻辑”可能冲突，实施前需要用户确认 |

会话中提过、用户未回复、本次又明确排除的事项：usage 前后一致性（不改）、代码执行（不改）。签名占位值没有被要求，按“不伪造”处理。

## 下一步执行清单

1. [ ] 进入 `/Users/yuanfeijie/Desktop/procode/2ue_kiro.rs-compat`，确认 `feat/protocol-compat` 仍基于最新 `main`，如果有新提交先 rebase。
2. [ ] 重启隔离实例 `127.0.0.1:19322`（启动前先检查是否已在运行），跑一遍 `artifacts/probe.py` 确认基线没有变化。
3. [ ] P1：查齐所有 tool_use id 的生成和发出点，实现确定性映射，补单元测试。
4. [ ] P2：在 converter 中解析 `output_config.format`，注入内部工具；在流式和非流式响应里还原成 JSON 文本，补单元测试。
5. [ ] 查 `ba720d1` 的删除原因（Q3），等用户回复 Q1、Q4。
6. [ ] P3：实现 WebSearch 开关和模型驱动流程，补单元测试。
7. [ ] 真实账号回归、本地 Claude Code 多轮验证、单跑一次全量测试。
8. [ ] 提交 commit（不 push），停掉测试服务，清理 `/tmp` 下的临时产物。
9. [ ] 用户确认后合并到 `main` 并发布；在 cctest 复测，结果补到本目录。

## 产物索引

| 文件 | 说明 |
|---|---|
| `artifacts/probe.py` | 逐项协议探针，覆盖结构化输出、工具、WebSearch、签名、流式 usage；从 `/tmp/kiro-identity-e2e/config.json` 读取 key |
| `artifacts/bank.py` | 题库回归，用法 `bank.py <label> <rounds> <model> [case,...]` |
| `artifacts/bank-sonnet45.json` | 改造前的基线数据 |
| `artifacts/三家去重题库.txt` | cctest、ztest、hovy 三家去重题库副本 |

原件在 `/tmp/kiro-cctest/`，重启后可能被清掉，以本目录为准。

## 用户决定（2026-10-20）

- D1：WebSearch 新模式默认关闭，按路由开启。
- D2/D3：额外 Kiro 调用的 token 按官方口径累加进 usage（本项允许改 usage）。
- D4：查清 `web_search_requests` 当初为何影响 usage；在不影响最终 usage 整形能力的前提下恢复或优化写法。
- D5：带用户工具时，让模型在用户工具和输出 JSON 之间自选。
- D6：做完提交，不合并，等用户验收。

## 执行结果（2026-10-20）

| 项 | 结果 |
|---|---|
| P1 工具 id | `toolu_01…`（Kiro id 哈希映射，分片一致），`srvtoolu_01…`；第二轮 tool_result 回传正常 |
| P2 结构化输出 | 流式和非流式都返回纯 JSON text，`end_turn`；带用户工具时模型自选 |
| P3 WebSearch | 路由开关 `bodyConversion.modelDrivenWebSearch`，默认关；开启后模型生成搜索词、回答带 citations；usage 累加所有模型调用后再整形 |
| D4 | `ba720d1` 删 `web_search_requests` 是随 sub2api usage 清理一起删的；sub2api 和 usage 记录只读四个 token 字段，已在整形后恢复到最终 usage，两种模式都有 |
| 题库回归 | 三种强制工具 12/12，id 均为 `toolu_01`；json 4/4；echo_tag 仍有 1/4 漏系统提示（需开身份隐藏输出过滤）；code_exec 仍无结果（按要求未改） |

验收时：在路由策略页给测试路由打开“模型驱动搜索”，用 cctest 复测。
