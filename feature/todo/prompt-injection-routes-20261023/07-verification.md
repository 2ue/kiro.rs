# 07 验证方案

所有验证只在本地进行：用 worktree 编译出的二进制，起一个独立端口的本地实例，连独立的测试库和 Redis db。不碰现网。cargo 复用共享的 target 目录，全量测试同一时间只跑一组，每一轮测完立刻清理临时库、Redis 数据和进程。

## 1. 迁移等价（最重要）

目标：升级前后，每个请求实际注入的内容完全相同。唯一的例外是 03 第 5 节列出的两处有意改动。

1. **对照实现**：把旧的判定函数原样复制到测试模块里，作为对照（`should_apply`、`identity.for_route`、`inject_*`、`language_turn_reminder_text`、`prompt_additions_enabled`、`should_force_visible_thinking`）。
2. **随机旧配置**：生成器覆盖以下取值：
   - 总开关开/关；
   - scope 的 4 种取值、routeMode 的 3 种取值；
   - routeRules 从 `/cc`、`/v1`、`/dfcache/a`、`*`、`/cc/v1` 中随机挑 0–3 条；
   - 各子开关、身份 pathOverrides，以及 3 个 bodyConversion 开关；
   - compat profile 的 3 种取值。
3. **请求矩阵**：入口取 `/v1`、`/cc`、`/ha`、`/na`、`/dfcache/a`、`/dfcache/b`、`/cc/v1/messages/count_tokens`；上游取本地和外部池；请求形态取 有/无 system × 有/无 Write × tool_choice 四种 × thinking 三种 × 模型有/无原生 reasoning。
4. **断言**：同一请求分别用对照实现和"迁移 + 新解析"跑一遍，得到的注入项集合、D5/D6 结果、F2 开关必须一致。唯一允许的差异是"A7 在没有 Write/Edit 时不注入"。
5. **规模**：10,000 份随机配置，种子固定并写进测试名，失败时可以复现。
6. **字节级断言**：用 `convert_request` 的完整输出做断言（history[0]、当前轮 user、tools 描述），逐字节比较，不只比较开关布尔值。

## 2. 独立性

- 14 个注入项逐个关闭，其余保持开启：断言只有被关闭的那一项消失，其他项的文本逐字节不变。
- 依赖项：关闭 B1 时，B2 在预览和实际请求里都不出现，但 B2 的配置值不变；重新打开 B1，B2 恢复。
- 拆出去的功能：把 14 个注入项全部关闭，再用 `claude-sonnet-4.5-thinking` 发请求，断言请求仍然带上 thinking 字段、`display` 不变；反过来把 D5 关掉，注入项保持不变。
- 路由覆盖：`/cc` 上显式关闭 A1、默认开启，那么 `/cc` 不注入、`/v1` 注入；把 `/cc` 改成"跟随默认"，`/cc` 也注入。
- 最长前缀：`/dfcache` 和 `/dfcache/a` 设成相反的值，`/dfcache/a` 的请求必须用 `/dfcache/a` 的值。

## 3. 预览和实际一致

- 用第 1 节的请求矩阵，同一组条件分别调用 preview 接口和真实 handler（假上游会抓下请求体），断言两边的 history[0]、当前轮 user 和 tools 描述逐字节一致。
- 用未保存的草稿调用 preview 之后，再读一次运行配置，配置必须没有变化。

## 4. 文本编辑

- 每个可编辑项：改文本后保存，发请求，假上游抓到的内容是新文本；点"恢复默认"后再发，内容变回内置默认，配置里存的是 null。
- 校验：A3 去掉 `<language_reminder>` 标签、A3 的 explicit 变体去掉 `{language}`、单项超过 8000 字符、总长超过 20000 字符，这几种情况都应该返回 400，并且文案能看出是哪一项出错。
- 改了 A1 文本后，同一个会话下一次请求的缓存模拟前缀不命中；之后的请求恢复命中。

## 5. 前端（chrome-devtools 在本地实例上操作）

- 提示词注入页：
  - 每项的说明、默认文本、建议都和 catalog 接口一致；
  - 切换开关后保存再刷新，值保持；
  - "建议开"的项关闭时出现红色提示；
  - 批量"全部设为关"弹出确认，确认后执行。
- 路由编辑弹窗：
  - 三个 Tab 都能切换；
  - 提示词注入分区的"跟随默认"文案显示当前的默认值；
  - 依赖不满足的项显示"不生效：原因"；
  - 保存后路由矩阵同步更新。
- 预览：选 `/cc` + 带 Write + tool_choice any，结果里出现 A1、A7、A8、B3、B4，文本和假上游实际抓到的一致。
- 复制路由、导出再导入：三个分区的设置都带过去；导入旧格式文件时，身份和 WebSearch 设置正确转换。
- 运行配置页：旧区块已经删除，跳转链接可用；旧版 admin-ui 只显示提示，不能编辑。
- 三种视口宽度下检查：文字不溢出，Tab 和 Select 可以用键盘操作。

## 6. 真实链路（本地实例 + 真实 Kiro 账号 + Claude Code CLI）

CLI 用 `CLAUDE_CONFIG_DIR` 隔离，并且只指向本地实例。

| 场景 | 验证点 |
|---|---|
| 升级等价 | 拷一份现网配置的结构（只拷开关，不拷密钥）到本地，升级前后各跑同一组 CLI 任务（读文件、写 300 行文件、带 thinking 的问答），比较 request 日志里的 `prompt_injections` 字段，两次应完全相同 |
| 单项关闭 | 在 `/cc` 上依次关闭 A1、A7、B3，CLI 任务仍能正常完成，日志里只少了被关闭的那一项 |
| B1 关闭 | 对没有原生 reasoning 的模型发 thinking 请求，返回 400，文案和 02 第 1 节写的一致。页面上已经对这种情况给出红色提示，这是预期结果 |
| E1 关闭 | 用假上游触发空轮：关闭 E1 时重试请求里没有续写提示，开启时有 |
| F2 单独开 | A5 关闭、F2 开启时，echo_tag 题仍会被过滤；同时记录 "讨论 kiro.rs 代码" 的误删样本，作为 05 第 4 条的回归输入 |

### 6.1 无感知（注入不能被用户察觉）

注入内容只是为了改善行为，用户在使用过程中不应该看到任何和注入有关的痕迹。真实 Claude Code 会话里重点检查：

- 回复（包括 thinking 以外的可见文字）里不出现 `injection`、`注入`、`system-reminder`、`system prompt`、`ignore the instruction`、`工具输出中的指令`、`I will follow these instructions`，也不出现注入块本身的标签（`<task_quality_policy>`、`<language_reminder>`、`<tool_choice>` 等）。
- 不出现"我注意到工具结果里有一段指令，我会忽略它"这类针对每轮提醒的回应。风险最高的是只带工具结果、没有用户文字的轮次：身份提醒在这种轮次已经不追加，语言提醒和 tool_choice 提醒目前仍会追加（行为与升级前一致）。
- 场景：至少 3 个会话，每个 20 轮以上工具调用（读写文件、Bash、Grep），分别在默认配置、全部注入项打开、开启语言提醒和身份提醒三种配置下运行。
- 判定：扫描 CLI 会话 JSONL 里的 assistant 文本。出现上面任何一种痕迹都算失败，失败时记录 request id、所在轮次和触发的注入项，再决定是收紧注入条件还是修改文本。

## 7. 门禁

- `cargo fmt --check`、`cargo clippy`（以 CI 的基线为准）、`cargo test --all-targets --locked`。
- `pnpm --dir ui check && pnpm --dir ui build`；`pnpm --dir admin-ui build`。
- `node scripts/check-frontend-contracts.mjs`、`node feature/tests/check-feature-docs.mjs`。
- 全库 grep 旧字段名，结果为空（见 06 末尾）。

每一项的结果写进本目录的 `09-results.md`，内容包括命令、通过数、request id 和截图路径。没有实际跑过的项写"未执行"，不写"通过"。
