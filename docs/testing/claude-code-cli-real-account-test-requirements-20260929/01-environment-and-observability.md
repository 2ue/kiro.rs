# 01 环境准备与观测

本分册原先只有预检、配置、命令封装和观察点说明，没有编号用例。本次新开 ENV 组，编号前缀为 `CC-ENV-`（CC-ENV-01 ～ CC-ENV-30），用例集中在第 7 节。第 1 ～ 6 节是所有分册共用的环境约定，其他分册的用例默认已满足这些约定。

路径变量（由操作员在当前 shell 中设置，本文档不写死具体位置）：

| 变量 | 含义 |
| --- | --- |
| `REPO_ROOT` | 本仓库（kiro-rs）根目录 |
| `KIRO_CONFIG` | 19023 测试实例使用的配置文件（位于仓库内不入库的本地目录） |
| `KIRO_STATE_DIR` | 仓库内不入库的本地状态目录，存放服务日志、证据和临时脚本 |
| `CARGO_SCOPED_WRAPPER` | 仓库提供的 scoped Cargo 包装脚本，构建必须经过它，避免与其他构建共用 target 目录 |

## 1. 服务预检（每轮开始前必做）

```bash
cd "$REPO_ROOT"
PORT=19023
lsof -nP -iTCP:$PORT -sTCP:LISTEN            # 是否已在运行
pid=$(lsof -nP -iTCP:$PORT -sTCP:LISTEN -t | head -n1)
ps -p "$pid" -o command=                      # 必须是本仓库 kiro-rs，且 -c "$KIRO_CONFIG"
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:$PORT/healthz
curl -s http://127.0.0.1:$PORT/readyz         # Postgres / Redis / runtime events 均 ready
lsof -nP -iTCP:9022 -sTCP:LISTEN              # 记录日常端口状态，本轮不得向其发请求
```

决策：

| 情况 | 处理 |
| --- | --- |
| 19023 在运行，且进程/配置/二进制就是待测版本 | 直接复用，不重启 |
| 19023 在运行，但不是待测构建 | 确认 PID 属于本仓库 kiro-rs 后只停这一个进程，用同端口、同配置启动待测二进制 |
| 19023 未运行 | 按下方方式启动，并记录为"本轮启动" |
| 19023 被其他项目占用 | 停止测试，报告冲突，不得占用或杀掉别人的进程 |

常规测试所有用例复用同一个 19023 实例，不为每个用例另起代理实例。只有破坏性用例无法在共享实例上安全执行时才另起临时实例，并记录端口、配置、存储（数据库、Redis namespace）和清理结果。

构建与启动（Cargo 必须经 scoped wrapper，二进制拷出后记录 SHA-256）：

```bash
candidate_root="$(mktemp -d "${TMPDIR:-/tmp}/kiro-cli-candidate.XXXXXX")"
KIRO_FROZEN_BINARY="$candidate_root/kiro-rs" \
  "$CARGO_SCOPED_WRAPPER" cc-real-account -- \
  bash -lc 'cargo build --release && install -m 755 "$CARGO_TARGET_DIR/release/kiro-rs" "$KIRO_FROZEN_BINARY"'
shasum -a 256 "$candidate_root/kiro-rs"
nohup "$candidate_root/kiro-rs" -c "$KIRO_CONFIG" \
  > "$KIRO_STATE_DIR/kiro-rs-19023-cc-real-$(date +%Y%m%d%H%M).log" 2>&1 &
```

替换二进制后，先确认 `/healthz` 返回 200、`/readyz` 全部 ready，再跑 CLI。

测试结束：

- 本轮启动的 19023 实例：按闭环要求决定保留（下轮回归继续用）或停止；若停止，只 kill 已核验的 PID，并再次 `lsof` 确认端口释放。替换过二进制的，结束时恢复原二进制并再次通过 `/readyz`。
- 本轮启动的其他进程（MCP server、http/sse MCP、抓包代理、fake server、`claude --bg` 后台会话、tmux 会话）全部停止并确认。
- 删除本轮的隔离 CLI 目录、临时 Cargo target、测试用 Redis 前缀和原始抓包（先保存脱敏摘要和哈希）。
- 如果本轮用过服务商切换工具（如 ccman）切换 profile，切回原 profile 并核对。

## 2. 账号与运行配置预检

用 Admin API（`x-api-key: <admin key>`，值不落盘到文档）记录：

| 检查项 | 接口 | 记录内容 |
| --- | --- | --- |
| 账号列表与状态 | `GET /api/admin/credentials-paged` | 账号数量、id、类型（social/idc/…）、plan、disabled、failure_count |
| 余额 | `GET /api/admin/credentials/{id}/balance` | 开始/每组结束/结束时的剩余额度 |
| 单账号冒烟 | `POST /api/admin/credentials/{id}/test` | sonnet-4.5、haiku-4.5 各一次，确认账号可用 |
| 模型目录 | `GET /api/admin/model-capabilities` | 上游实际可用模型、是否带原生 reasoning 能力（决定 thinking 走原生字段还是 XML 提示） |
| 运行配置 | `GET /api/admin/config/runtime` | `thinkingTriggerMode`、`payloadTooLongHandling`、`payloadGuard*`、`payloadShaping.*`、`bodyConversion.*`、`reportedUsage`、外部池开关、`requestAdmission`、`kiroUpstreamStreamIdleTimeoutSecs`、`expose_proxy_warnings`、后台 token 刷新开关 |

基线要求（除非用例明确要求修改）：

- `thinkingTriggerMode=real_request`
- `payloadTooLongHandling=client_compaction`（当前默认值：上游报 too-long 时把错误交给 Claude Code 自己压缩，不在服务端静默裁剪）
- 外部池关闭或无可用外部池
- 修改运行配置的用例必须先备份当前配置 JSON，用例结束后恢复并再次 GET 确认。

## 3. Claude Code 指向本地服务

### 环境变量

| 变量 | 取值 | 说明 |
| --- | --- | --- |
| `HOME` | `/tmp/kiro-cc-home-19023-$RUN_ID` | 隔离，避免读到本机 `~/.claude` |
| `CLAUDE_CONFIG_DIR` | `/tmp/kiro-cc-config-19023-$RUN_ID` | settings.json、会话记录、.claude.json 都在这里 |
| `ANTHROPIC_BASE_URL` | `http://127.0.0.1:19023/cc` | 主入口；对照用例改为 `/`、`/na`、`/ha`（CLI 会自己拼 `/v1/messages`） |
| `ANTHROPIC_API_KEY` | 测试 key（`<redacted>`） | `--bare` 模式只认这个 |
| `ANTHROPIC_AUTH_TOKEN` | 测试 key | 非 bare 模式的另一种方式，以 `Authorization: Bearer` 发送；服务端 `x-api-key` 和 Bearer 两种都支持，CC-E 组各测一次。注意 `--bare --print` 下只设它不生效（见 CC-ENV-01） |
| `ANTHROPIC_MODEL` | `claude-sonnet-4-5` | 主模型；等价于 `--model` |
| `ANTHROPIC_DEFAULT_SONNET_MODEL` | `claude-sonnet-4-5` | 让 `sonnet` 别名和 `/model` 菜单落到免费 plan 可用模型 |
| `ANTHROPIC_DEFAULT_HAIKU_MODEL` | `claude-haiku-4-5` | 后台小模型（标题、摘要、WebFetch 处理等） |
| `ANTHROPIC_SMALL_FAST_MODEL` | `claude-haiku-4-5` | 旧变量名，新版本已被上一项取代；两者同时设置，并在 CC-M 组验证哪个生效 |
| `ANTHROPIC_DEFAULT_OPUS_MODEL` | 不设置（或设为 `claude-sonnet-4-5`） | 防止 `opus`/`opusplan` 打到不可用模型；CC-M-05 专门测不设置的行为 |
| `CLAUDE_CODE_SUBAGENT_MODEL` | 按用例 | 子代理模型，CC-G 组使用 |
| `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` | `1`（默认） | 关闭遥测等非必要流量；CC-M-07 需要去掉它来观察后台 haiku 请求 |
| `DISABLE_AUTOUPDATER` | `1` | 避免测试中 CLI 自升级改变版本 |

变量名以当前 CLI 版本为准。每轮开始时用 `claude --version` 记录版本（CC-ENV-05），并在 CC-M 组确认上述变量实际生效（看服务端收到的 model 字段）。

CLI 可执行文件：如果 `claude` 由 Volta 等版本管理器提供，先用 `volta which claude`（或同类命令）拿到真实二进制路径，用该路径运行（CC-ENV-04）。

### settings.json（`$CLAUDE_CONFIG_DIR/settings.json`）

基线：

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:19023/cc",
    "ANTHROPIC_MODEL": "claude-sonnet-4-5",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "claude-sonnet-4-5",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "claude-haiku-4-5"
  },
  "model": "claude-sonnet-4-5",
  "permissions": { "allow": [], "deny": [] }
}
```

key 不写进 settings 明文，用环境变量或 `apiKeyHelper`（输出 key 的脚本，脚本文件不入库）。用例要改的字段（`alwaysThinkingEnabled`、`autoCompactEnabled`、`hooks`、`permissions`、`statusLine` 等）在用例里写明，用例结束恢复基线。

也要覆盖一次"只用 settings.json 的 env 而不设 shell 环境变量"的方式（CC-A-09），确认两种配置路径等价。

如果运行环境中存在用户级全局 settings 且其 `env` 配置了别的 `ANTHROPIC_BASE_URL`（例如未隔离 HOME 的场景），每条命令加 `--setting-sources project,local`，否则 CLI 会把请求发到全局配置里的第三方服务（CC-ENV-02）。

### 交互式首次启动

隔离 HOME 下首次交互式启动会出现主题选择、信任目录、"是否使用该 API key"确认。首次手工完成后该隔离目录可在本轮复用；每个 RUN_ID 重新做一次（CC-ENV-07）。

## 4. 命令封装

```bash
export RUN_ID=$(date +%Y%m%d-%H%M%S)
export EVID="$KIRO_STATE_DIR/cc-real-$RUN_ID"    # 证据目录，不入库
export WORK=/tmp/kiro-cc-work-$RUN_ID            # 临时工作目录（git init 一个小项目）
export CC_HOME=/tmp/kiro-cc-home-19023-$RUN_ID
export CC_CFG=/tmp/kiro-cc-config-19023-$RUN_ID
mkdir -p "$EVID" "$WORK" "$CC_HOME" "$CC_CFG"
# KIRO_TEST_KEY 由操作员在当前 shell 中导出，不写入任何文件
# CLAUDE_BIN 为真实 CLI 二进制路径（见 CC-ENV-04），默认 claude
export CLAUDE_BIN="${CLAUDE_BIN:-claude}"

ccenv() {
  env HOME="$CC_HOME" CLAUDE_CONFIG_DIR="$CC_CFG" \
    ANTHROPIC_BASE_URL="${CC_BASE:-http://127.0.0.1:19023/cc}" \
    ANTHROPIC_API_KEY="$KIRO_TEST_KEY" \
    ANTHROPIC_MODEL="${CC_MODEL:-claude-sonnet-4-5}" \
    ANTHROPIC_DEFAULT_SONNET_MODEL=claude-sonnet-4-5 \
    ANTHROPIC_DEFAULT_HAIKU_MODEL=claude-haiku-4-5 \
    ANTHROPIC_SMALL_FAST_MODEL=claude-haiku-4-5 \
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="${CC_NONESSENTIAL_OFF:-1}" \
    DISABLE_AUTOUPDATER=1 "$@"
}

# 非交互：stream-json + partial + debug log，CASE 为用例编号
ccp() {
  (cd "$WORK" && ccenv "$CLAUDE_BIN" -p --verbose \
     --output-format stream-json --include-partial-messages \
     --debug-file "$EVID/$CASE.debug.log" "$@") \
    > "$EVID/$CASE.jsonl" 2> "$EVID/$CASE.stderr"
  echo "exit=$?" > "$EVID/$CASE.exit"
}

# 交互式：在 tmux 中运行，便于脚本化发送按键并留存屏幕
cci() {
  tmux new-session -d -s "cc-$CASE" -x 200 -y 50 \
    "cd $WORK && $(declare -f ccenv); ccenv $CLAUDE_BIN --debug-file $EVID/$CASE.debug.log $*"
  tmux pipe-pane -t "cc-$CASE" -o "cat >> $EVID/$CASE.tty.log"
}
# 发送: tmux send-keys -t cc-$CASE 'prompt' Enter
# 截屏: tmux capture-pane -p -t cc-$CASE > $EVID/$CASE.screen.N.txt
# 中断: tmux send-keys -t cc-$CASE Escape  或  C-c
# 结束: tmux kill-session -t cc-$CASE

# 直连协议（不经 CLI），只用于对照：响应头写入 .headers，SSE 原文写入 .sse
# 用法: CASE=CC-ENV-16 cccurl -d @"$EVID/body.json"
cccurl() {
  curl -sS -N -D "$EVID/$CASE.headers" \
    -H "x-api-key: $KIRO_TEST_KEY" \
    -H 'anthropic-version: 2023-06-01' \
    -H 'content-type: application/json' \
    "${CC_BASE:-http://127.0.0.1:19023/cc}/v1/messages" "$@" \
    > "$EVID/$CASE.sse"
  echo "exit=$?" > "$EVID/$CASE.exit"
}
```

`--bare` 只在需要最小化干扰时使用（它跳过 hooks、CLAUDE.md、插件同步、自动记忆和后台预取），hooks、Skills、插件、后台 haiku 请求类用例不得加 `--bare`。

`cccurl` 的结果只能证明代理协议本身；任何用例都不能用 curl 或 mock 的结果代替真实 CLI 的结论（CC-ENV-28）。

## 5. 观察点

| 层 | 位置 | 看什么 |
| --- | --- | --- |
| CLI 输出 | `$EVID/$CASE.jsonl` | `system/init`（model、tools、mcp_servers 状态）、`stream_event`（`message_start`、`content_block_start` 类型、`thinking_delta`、`signature_delta`、`text_delta`、`input_json_delta`、`message_delta.stop_reason/usage`）、`assistant`、`user`（tool_result）、`result`（subtype、num_turns、usage、modelUsage、is_error） |
| CLI debug | `$EVID/$CASE.debug.log` | API 请求重试、错误原文、压缩触发（`trigger=auto`）、MCP 连接与工具耗时、hook 执行 |
| CLI 会话 | `$CC_CFG/projects/<工作目录>/*.jsonl` | 多轮历史里 thinking/signature/tool 块是否完整保存；运行中追加的输入是 user 消息还是队列操作 |
| 服务日志 | `$KIRO_STATE_DIR/kiro-rs-19023-*.log` | request id、选中账号 id 与类型、requested/upstream model、payload guard / shaping 动作、too-long 处理、重试（attempts 列表）、上游错误分类、stream idle、client drop |
| 响应头 | `request-id`、`anthropic-request-id`、`x-kiro-rs-warnings` | 每个请求都应返回 request id，并能在服务日志和 usage 记录中查到；warnings 头见 CC-ENV-24 |
| usage 记录 | `GET /api/admin/usage-records-paged` | 按时间窗查询本轮请求：endpoint、model/upstreamModel、status、raw usage 与 reported usage、latency trace、attempts、错误摘要与诊断类别。本地预检拒绝的记录是采样，不是精确计数 |
| 账号状态 | `GET /api/admin/credentials/runtime` | in-flight、冷却、失败计数是否在用例后回落 |
| 抓包（可选） | 在 CLI 与 kiro.rs 之间放一个本地反向代理，如 `mitmdump --mode reverse:http://127.0.0.1:19023 -p 19123 -w $EVID/$CASE.flow`，CLI 指向 19123 | 客户端真实发出的 `thinking`、`output_config.effort`、`tools`、`tool_choice`、`anthropic-beta`、`max_tokens`、`temperature`、`stop_sequences`、`context_management` 等字段；CLI 启动时的 `HEAD /cc` 探测；以及服务端 SSE 原文。抓包文件含 key 和完整 prompt，只在本机临时保存，写报告前提取摘要后删除 |

## 6. 证据要求

每条用例至少保存：命令（key 脱敏）、退出码、`result` 行、request id 列表、关键事件计数（thinking block/delta、signature、tool_use/tool_result、text）、最终 usage、服务日志中对应 request id 的摘录（脱敏）。交互式用例额外保存关键时刻的 `capture-pane` 文本。

每轮还要记录：CLI 版本、代理版本或二进制 SHA-256、实际使用的 base URL。

原始 `.jsonl` / debug log / 抓包只放在 `$EVID`（不入库的本地目录）。写入问题记录或测试报告的只能是脱敏摘要：stdout 的 SHA-256、行数、字节数、可疑行摘要、状态码和 request id。持久化证据里不得出现 API key、refresh token、cookie、完整凭据 JSON、完整 prompt、工具内容或文件正文。

## 7. ENV 组用例

未注明模型时为 `claude-sonnet-4-5`。"来源"取值见 README 用例约定。

### 7.1 隔离与接入

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-ENV-01 | P0 | 既有场景 | 第 1 节预检通过；`KIRO_TEST_KEY` 已导出；记录本机 `~/.claude` 目录与日常 9022 端口的初始状态 | 1. 按第 4 节创建隔离目录；2. `ccenv "$CLAUDE_BIN" --version`；3. `CASE=CC-ENV-01 ccp --bare 'Reply with exactly: pong'`；4. 对照：`env HOME="$CC_HOME" CLAUDE_CONFIG_DIR="$CC_CFG" ANTHROPIC_BASE_URL=http://127.0.0.1:19023/cc ANTHROPIC_AUTH_TOKEN="$KIRO_TEST_KEY" "$CLAUDE_BIN" --bare -p 'Reply with exactly: pong'`（不设 `ANTHROPIC_API_KEY`） | 独立 HOME / CLAUDE_CONFIG_DIR；每条命令单独注入 BASE_URL 与 API key；`--bare` | 步骤 3 退出码 0，服务日志出现 `/cc/v1/messages`；本机日常 CLI 的配置、会话和日常代理端口都没有被读写或占用；输出和日志中不打印 key、refresh token 或凭据内容。步骤 4 提示 `Not logged in · Please run /login`，服务端收不到任何请求，这是 CLI 的预期行为，不记为代理问题 | 比较 `~/.claude` 前后 mtime；9022 本轮日志/usage 无新增；在 `$EVID` 中搜索 key 前缀命中 0。背景：所有真实 CLI 验证都必须隔离，避免污染日常环境；不能把请求发到其他项目的服务端口。已知现象：只靠配置切换工具写入 `ANTHROPIC_AUTH_TOKEN` 时，`--bare --print` 不认这个登录态 |
| CC-ENV-02 | P0 | 既有场景 | CC-ENV-01 通过 | 1. 在 `$CC_CFG/settings.json` 的 `env` 中把 `ANTHROPIC_BASE_URL` 改成一个不可达地址（如 `http://127.0.0.1:1/cc`），模拟"全局 settings 指向第三方服务"；2. `CASE=CC-ENV-02a ccp --bare 'Reply with exactly: pong'`；3. `CASE=CC-ENV-02b ccp --bare --setting-sources project,local 'Reply with exactly: pong'`；4. 恢复基线 settings | `--setting-sources project,local`；`CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` | 02a 的请求不到 19023（连接失败或打到 settings 指定地址）；02b 退出码 0，请求到达 19023 | 以 19023 服务日志 / usage 中是否出现对应 request id 为准。背景：不加该参数时 CLI 会使用全局配置里的第三方服务，测试结论被污染。若 02a 也打到 19023，记录当前 CLI 版本下 settings 与 shell 环境变量的优先级 |
| CC-ENV-03 | P0 | 既有场景 | CC-ENV-01 通过 | 1. `CASE=CC-ENV-03a ccp --model sonnet '用一句话回答：1+1 等于几'`；2. `CASE=CC-ENV-03b CC_MODEL=claude-haiku-4-5 ccp '用一句话回答：1+1 等于几'`；3. 查 usage 记录与服务日志 | 模型别名 `sonnet`；显式 haiku；不加 `--bare`（带默认工具定义） | debug 日志显示请求打到代理的 `/cc/v1/messages`；模型别名映射成上游模型名（sonnet-4.5 / haiku-4.5）；CLI 默认工具定义进入转换链路且无转换错误；每轮 CLI 请求都能在 usage/日志里找到对应记录，路由类型为 `/cc`，上游模型与请求模型一致 | 每个会话/轮次记录 CLI 版本、二进制 SHA-256、实际 base URL、服务端 request id。背景：只看 CLI 有回复不够，必须排除 CLI 实际连到了别的端点或官方 API；本用例确认"服务商切换 → CLI → 代理路由 → 模型映射 → 工具转换"整条链路通 |
| CC-ENV-04 | P1 | 既有场景 | CLI 通过 Volta 或同类版本管理器安装 | 1. `volta which claude`（或同类命令）得到真实二进制路径，导出为 `CLAUDE_BIN`；2. 分别用 shim 命令 `claude` 和 `$CLAUDE_BIN` 跑 `ccp --bare 'Reply with exactly: pong'` | 真实二进制 vs shim | 用真实二进制时 CLI 正常启动并完成请求；shim 若启动失败，记录现象，后续所有用例一律用真实二进制 | 报告中写明所用二进制路径与版本。已知现象：通过 Volta shim 启动时 runner 出现启动失败，改用真实二进制后同样的用例通过 |
| CC-ENV-05 | P0 | 既有场景 | 无 | 每轮开始执行 `"$CLAUDE_BIN" --version` 并写入 `$EVID/cli-version.txt` | 可要求精确版本；否则只接受不低于支持下限 2.1.197 且能识别的版本 | 报告中写入实际版本；版本低于下限或无法识别时本轮判为失败，不继续执行 | 已知现象：不同 CLI 版本行为有差异（例如 2.1.280 没有触发原生 WebSearch），所有证据必须能对应到具体版本 |
| CC-ENV-06 | P1 | 既有场景 | 按第 5 节启动本地反向代理抓包，`CC_BASE=http://127.0.0.1:19123/cc` | `CASE=CC-ENV-06 ccp 'Reply with exactly: pong'`，从抓包中筛选 CLI 启动阶段的请求 | 抓包 | CLI 先发 `HEAD /cc`，代理按协议探测正确处理（返回成功状态），不记为错误或未知请求；随后的 `/cc/v1/messages` 正常 | 服务日志无该 HEAD 的 ERROR/WARN；usage 中 unknown 请求数为 0。背景：真实 CLI 入站抓包确认 CLI 会先发 `HEAD /cc` |
| CC-ENV-07 | P0 | 新增（用户要求） | 新 RUN_ID 的隔离目录 | 1. `CASE=CC-ENV-07 cci`；2. 依次完成主题选择、信任目录、"是否使用该 API key"确认，每步 `capture-pane`；3. 输入 `记住数字 42，只回复 ok`；4. 输入 `刚才让你记住的数字是多少？`；5. `/exit` | 交互式 tmux；非 bare | 首次引导可完成且不报错；两轮对话都成功，第 2 轮回答 42；同一隔离目录在本轮后续交互用例中不再出现引导 | 屏幕无 API 错误；服务端两次 `/cc/v1/messages` 均成功，第 2 次请求带上第 1 轮历史；会话 jsonl 中两轮完整 |

### 7.2 实例、账号与生命周期

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-ENV-08 | P0 | 既有场景 | 无 | 测试前后各执行第 1 节预检脚本：`lsof` 找 PID，`ps -p <pid> -o command=` 确认是本仓库 kiro-rs，配置为 `$KIRO_CONFIG`，数据库与 Redis namespace 归属正确；替换二进制时按第 1 节决策表处理，替换后先查 `/healthz`、`/readyz` 再跑 CLI | 19023 | 只停止经核验的那一个进程；健康检查成功；所有用例复用同一实例；结束后恢复原二进制并通过 `/readyz` | 记录 PID、命令行、二进制 SHA-256、"复用/本轮启动/替换"。背景：常规测试不允许为每个用例另起代理实例；破坏性用例需要临时实例时，记录其端口、配置、存储和清理结果 |
| CC-ENV-09 | P0 | 新增（补充） | CC-ENV-08 通过 | 按第 2 节调用 credentials-paged、balance、credentials/{id}/test（sonnet-4.5、haiku-4.5 各一次）、model-capabilities | Admin API | 至少一个免费 plan 账号可用；两个模型冒烟均成功；记录每个模型是否带原生 reasoning 能力 | 余额写入 `$EVID/balance-start.json`；原生 reasoning 状态决定 03 分册签名类用例是否适用（无原生能力时 thinking 走 XML 提示路径，不会有 `signature_delta`，属预期） |
| CC-ENV-10 | P0 | 新增（补充） | CC-ENV-08 通过 | 1. `GET /api/admin/config/runtime` 备份到 `$EVID/runtime-config.base.json`；2. 核对第 2 节基线；3. 每个修改配置的用例结束后 PUT 回备份并再次 GET 比对 | Admin API | 基线满足；每次恢复后配置与备份逐字段一致 | 不一致时后续用例暂停，先恢复。备份文件含内部配置，不入库 |
| CC-ENV-11 | P1 | 既有场景 | CC-ENV-10 已备份配置 | 1. 不重启服务，在管理端修改路由策略（缓存、usage 上报、prompt steering 任选其一，每次只改一项）；2. `CASE=CC-ENV-11-<项> ccp 'Reply with exactly: pong'`；3. 恢复配置后再跑一次 | 运行时配置热更新 | CLI 请求按新配置生效（如 usage 上报口径、steering 是否注入可在 usage 记录/warnings 头/日志中看到变化），而不是按写死的路径规则走；恢复后回到基线行为 | 对比修改前、修改后、恢复后三次请求的 usage 记录和日志。背景：路由策略已改为由配置决定，但真实 CLI 下动态配置生效尚未验证 |
| CC-ENV-12 | P2 | 既有场景 | 运行配置中可开启闲置账号后台 token 刷新；CC-ENV-10 已备份 | 1. 开启后台刷新；2. 等待至少一个扫描周期（看服务日志确认刷新已执行）；3. `CASE=CC-ENV-12 ccp --bare 'Reply with exactly: pong'`；4. 期间记录服务进程 RSS 与 FD 数；5. 恢复配置 | 后台 token 刷新 | 文本正确；最终 usage 非 0；输出无内部调度术语；RSS/FD 不持续增长 | `ps -o rss= -p <pid>`、`lsof -p <pid>` 行数前后对比。背景：后台 worker 刷新账号不能影响请求热路径 |
| CC-ENV-13 | P0 | 既有场景 | 每组或每轮结束时 | 1. 停止本轮启动的 fake 上游、MCP server、抓包代理、tmux 会话、`claude --bg` 会话；2. 按需停止本轮启动的 19023 实例（只 kill 已核验 PID）；3. 检查端口监听、临时进程、临时 CLI HOME、临时 Cargo target、测试数据库/Redis 前缀；4. 用过 ccman 的切回原 profile | 清理 | 无残留端口监听、进程和临时目录；原 profile 已恢复并核对 | 清理结果写入本轮报告。临时服务、Cargo target、Redis 前缀任一没清理干净，也算门禁失败 |

### 7.3 流式、保活与时间线

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-ENV-14 | P0 | 既有场景 | CC-ENV-01 通过 | `CASE=CC-ENV-14 ccp --bare --no-session-persistence --model sonnet 'Reply with exactly: pong'` | `--bare`、stream-json、partial、`--no-session-persistence`、`--debug-file` | 退出码 0，最终文本为 `pong`；事件依次有 `system`、`stream_event`（`message_start`、内容块、`message_delta`、`message_stop`）、`assistant`、`result`；`result.usage` 非 0，含 `input_tokens`、`output_tokens`、`cache_creation_input_tokens`、`cache_read_input_tokens`、`service_tier` 等字段，有 `modelUsage`；响应带 request id；服务端能查到对应 request id 和 usage 记录 | 记录首包延迟与首个可见文本延迟。背景：用来确认链路、认证和 usage 回传正常；直接 curl 只能证明代理协议本身，只有真实 CLI 才能证明客户端能正确消费；测试不得改动操作者平时使用的 Claude/ccman 配置 |
| CC-ENV-15 | P0 | 既有场景 | CC-ENV-14 通过 | `CASE=CC-ENV-15 ccp --bare --no-session-persistence '写一段约 300 字的说明，介绍 TCP 三次握手'` | stream-json + partial | JSONL 里持续出现 `stream_event`（`message_start`、`content_block_start/delta/stop`、`message_delta`、`message_stop`），`text_delta` 在过程中逐步出现，而不是最后一次性聚合 | 给每行加接收时间戳（如经 `ts` 或脚本读取），统计 delta 数量与时间分布。背景：排查卡顿或突然结束时，要把 CLI 时间线、服务端原始 SSE 和 usage 记录按同一时刻对齐，才能判断问题出在哪一层 |
| CC-ENV-16 | P0 | 既有场景 | 准备最小请求体 `body.json`（model=claude-sonnet-4-5，stream=true，max_tokens 较小，一条 user 消息） | `CASE=CC-ENV-16 cccurl -d @"$EVID/body.json"` | 直连 `/cc/v1/messages`，stream=true | HTTP 200；响应头有 `content-type: text/event-stream`、`x-accel-buffering: no`、`request-id`、`anthropic-request-id`；事件顺序为 `message_start` → `content_block_*` → `message_delta` → `message_stop`，没有 `error` | 检查 `$CASE.headers` 与 `$CASE.sse`。背景：以前缺少 `x-accel-buffering: no`，Nginx 这类反向代理可能把 SSE 攒起来，输出变成一块一块地冒出来。本用例只作协议对照，不代替 CLI 用例 |
| CC-ENV-17 | P1 | 既有场景 | CC-ENV-15 通过；可选抓包 | `CASE=CC-ENV-17 ccp '先仔细思考一个复杂问题再回答：设计一个支持百万并发的限流器，给出最终方案要点'`（选首个输出较晚的任务，如长推理） | stream-json + partial；可选抓包 | `message_start` 立即发出；长时间无内容块期间持续有 `ping`；CLI 不因空闲超时断开，最终正常结束 | 抓包或服务日志中 `message_start` 到首个 `content_block_start` 的间隔，以及其间 ping 的间隔。背景：取消"预开空 text 块"后，首个 `content_block_start` 会推迟到模型首次真实输出，需要回归保活行为 |
| CC-ENV-18 | P1 | 既有场景 | 交互式；服务日志开启 latency trace；可选抓包 | `CASE=CC-ENV-18 cci`，输入一个会先长时间思考或调用工具的任务，记录回车时刻并定时 `capture-pane` | 交互式 | 能为同一请求拼出完整时间线：用户回车、服务端收到请求、调度开始/结束、发出上游请求、上游响应头、上游首个 chunk、首个 `thinking_delta`、首个可见 `text_delta`、首个 `tool_use`、首次向 CLI flush、CLI 界面首次可见变化 | 定位规则：上游首 chunk 就晚 → 上游、调度或网络问题；thinking 早但可见文本晚 → 模型在思考或 CLI 未展示 thinking；可见文本早但界面晚 → 事件格式、flush 或渲染问题；工具或 Agent 运行很久无提示 → 进度展示问题。已知现象：输入后界面长时间无可见反馈，然后一次性输出一大段；SSE ping 只保活，不会在 CLI 里形成可见进度；解析 thinking 标签时可能把早期文本攒住不发 |
| CC-ENV-19 | P1 | 既有场景 | 交互式 | 1. `CASE=CC-ENV-19 cci`，发起一个长时间运行工具或 Agent 的任务（如"逐个读取并总结工作目录所有文件"）；2. assistant 仍在工作时追加一条新要求（如"另外最后用英文总结"）；3. 结束后检查 `$CC_CFG/projects/...` 下的 transcript 和服务端请求摘要 | 运行中追加输入 | 追加要求最终被处理，或能明确归因 | 若 transcript 中这条输入只是队列操作、后面没有正常的 user 消息，判定为 CLI 侧未消费；若进入了请求，服务端摘要应证明入口和转换后都保留了这条输入。已知现象：运行中追加的要求被 CLI 记成队列操作，没有转成后续模型请求，模型继续做旧任务并直接收尾 |

### 7.4 usage 与服务端对账

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-ENV-20 | P0 | 既有场景 | 适用于所有 CLI 用例 | 每个 CLI 用例结束后，按 request id 和时间窗查服务端 usage 与日志：request id、上游模型、选中账号类型、attempts 列表、错误分类、结果状态；必要时打开工具格式调试请求体采样 | 服务端对账 | 通过的用例返回预期文本且退出码 0，服务端记为本地成功；每次内部重试（同账号签名重试、malformed 重试、换号重试）的次数和类型都能追溯 | attempts 数大于 1 的请求单独列出并说明原因。背景：代理有多层静默兜底重试，只看 CLI 最终成功会掩盖问题；本地预检拒绝在 usage 里只是采样记录，不是精确计数 |
| CC-ENV-21 | P0 | 既有场景 | CC-ENV-09 确认模型有原生 reasoning 能力（否则本用例不适用，记录"不适用"及原因） | 执行 03 分册中的 thinking 多轮签名回放用例，检查每个回放请求的 attempts | 签名回放 | 真正的通过是一次上游请求就成功，而不是靠去掉 reasoning 的重试才成功 | attempts 中出现"签名无效后同账号去掉 reasoning 重试"或"格式错误后同账号去掉 reasoning 重试"即判未通过；日志中出现"多块 reasoning 冲突，先丢历史 thinking"也判未通过。背景：上游报签名无效或 Improperly formed request 时代理会剥离 reasoning 重试，本地转换遇到多块 reasoning 冲突时也会先丢历史 thinking，这些都会让回放失败看起来像成功 |
| CC-ENV-22 | P1 | 既有场景 | `CC_NONESSENTIAL_OFF=0`（放开后台 haiku 请求） | `CASE=CC-ENV-22 ccp '在工作目录新建 a.txt 写入 hello，再读出来确认'`（一次 `--print` 会触发工具循环多次请求和后台 haiku 请求） | 非 bare；sonnet 主模型 + haiku 后台 | `modelUsage` 等于本次所有 messages 请求原始 usage 按模型之和；`result.usage` 只对应最后一条主响应；服务端每条请求各有一条 usage 记录 | 逐条列出服务端记录并与 `modelUsage` 对账。背景：一次 `--print` 可能触发多条上游请求，只看最终 result 会低估成本。原场景用 Opus 验证，免费 plan 不可用，改用 sonnet + haiku |
| CC-ENV-23 | P1 | 既有场景 | 能构造一个进入上游前就被本地拒绝的请求（例如当前模型 reasoning 通道不可用时强制 reasoning，或 reasoning 转换失败） | 1. `CASE=CC-ENV-23 ccp <构造参数> 'hi'`；2. 查 usage 记录和后台拒绝记录 | 本地/入口拒绝 | 拒绝记录里有请求模型、解析后/上游模型、具体诊断类别（转换失败类型），而不是 `model=unknown`、`content=unknown` 或诊断为空；CLI 收到可读错误 | 采样记录不当作精确计数。若字段缺失，区分是请求还没解析完、转换失败后记录丢了字段，还是查询层采样或脱敏导致显示缺失。已知现象：本地拒绝的采样记录固定写成 `model=unknown`、诊断内容看不到，排查者误以为模型没传或模型不可用 |
| CC-ENV-24 | P1 | 既有场景 | CC-ENV-10 已备份；开启 `expose_proxy_warnings` | 1. 经 `/cc` 和 `/v1`（`CC_BASE=http://127.0.0.1:19023`）各发一轮普通对话，同时抓包记录 CLI 实际发送、但代理会忽略的字段（如 `temperature`）；2. 用 debug log 或抓包查看响应头 `x-kiro-rs-warnings`；3. 关闭配置重复 | `expose_proxy_warnings` 开/关 | 开启时头中列出 converter 改写、被忽略的请求字段（如 `ignored-field=temperature`）、提示词注入（`prompt_steering_applied=...`），内容与实际一致；关闭时不出现该头 | 与服务日志中的改写记录逐项对照。已知现象：该头只统计 converter 内部改写，不含被丢弃的字段和提示词注入，且默认关闭，客户端看不到代理做了哪些改动，排障时无法和官方行为对照 |

### 7.5 输出安全与证据

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-ENV-25 | P0 | 既有场景 | 适用于所有 CLI 用例 | 扫描每个用例的 stdout（`.jsonl`）、交互屏幕和 transcript，搜索内部标记：`Tool results provided`、`<function_results>`、`user Continue`、带哈希后缀的内部工具别名（`xxxHash<8位十六进制>`）、号池、外部池、上游池、调度器、凭据、私有调度器等术语 | 全量扫描 | 命中数为 0；usage 中 unknown 上游请求数为 0；用户输入或工具结果里本来就有的文字不算泄漏；保护机制命中时给出明确失败，而不是返回空白、只有空格或截掉合法结尾的"成功"结果 | 出现内部术语是立即停止测试的条件之一。已知现象：内部 transcript、工具历史、工具名哈希或工具输出会混进回答；保护机制命中后曾只返回空格或截掉合法结尾；早期 runner 报过公开输出泄漏警告 |
| CC-ENV-26 | P0 | 既有场景 | 写报告或问题记录前 | 从 `$EVID` 提取摘要：stdout 的 SHA-256、行数、字节数、可疑行摘要、状态码、request id；对待持久化文件搜索 key 前缀、`refresh`、`cookie`、凭据 JSON 特征 | 脱敏 | 持久化证据里没有 API key、refresh token、cookie、完整凭据 JSON、完整 prompt、工具内容或文件正文 | 搜索命中 0 才允许写入报告。背景：证据需要长期留存，不能带出敏感信息 |
| CC-ENV-27 | P0 | 既有场景 | 修复前后各有一个可用构建 | 1. 每个用例保存 CLI 的 stream-json 输出、debug 日志、服务端日志增量；2. 修改前后用相同的配置、账号、模型、prompt、MCP 配置和 CLI 参数各跑一次；3. 在 stream-json 中搜索 `assistant`、`result`、`error`、`tool_use`、`tool_result`、`server_error`、`bad_request` 并计数 | 前后对比 | 修改后同一用例重试次数不增加，不出现新的错误类型；目标问题的现象消失 | 前后计数表写入回归记录。背景：不能用 mock 代替真实 CLI，mock 只用来定位小函数和复现边界 |

### 7.6 判定口径与失败归类

| 编号 | 优先级 | 来源 | 前置 | 步骤/命令 | 参数 | 预期结果 | 判定/观察点 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CC-ENV-28 | P0 | 既有场景 | 每轮汇总时 | 核对每类问题是否都用真实 CLI 验证过，覆盖文本、thinking、Bash、Read、工具循环、MCP、搜索、子代理、图片、长会话/resume、错误恢复；逐条标记执行状态 | 通过口径 | 用 curl 或 mock 结果冒充真实 CLI 不算通过；只跑单轮 happy path、只看源码、只看某个关键词不再出现，都不算已修复；未执行、被跳过或清理不完整的一律记为未通过 | 上游用假 Kiro 时只能验证协议、流、thinking、工具调用、MCP 和 usage 字段，不能代表真实模型能力、图片识别或长文档理解质量；非交互 CLI 通过不能代替交互式多轮、MCP、并行 agent 和长历史签名 thinking 的验证 |
| CC-ENV-29 | P1 | 既有场景 | 任何用例失败时 | 按失败原文和服务端错误分类归类：429 可疑活动、账号被禁用或冷却、额度预扣失败、模型临时不可用、上游高负载、没有可用渠道 | 失败归类 | 这类失败归为"账号/配额压力"，不算协议失败；CLI 显示 `API Error: Request rejected (429) · Upstream temporarily rate limited` 归为账号压力；等待冷却或换号后重跑，重跑通过则原用例记"环境失败后重跑通过" | 同时记录 `credentials/runtime` 中的冷却与失败计数。背景：用免费账号测试时常因 429 或额度问题中断，容易被误判为请求格式错误 |
| CC-ENV-30 | P2 | 既有场景 | 在隔离 `$CC_CFG` 中额外配置一个必然启动失败的 MCP server（模拟全局 MCP 噪声），同时配置 05 分册的测试 MCP | `CASE=CC-ENV-30 ccp '调用测试 MCP 的 echo 工具，参数 hello'` | MCP 噪声 | debug 日志中出现失败 MCP 的初始化报错；测试 MCP 连接成功、工具调用成功、结果正确；判定为通过 | `system/init` 中 mcp_servers 状态：测试 MCP 为 connected，噪声 MCP 为 failed。背景：用户全局配置了其他 MCP 时，初始化失败会在日志里产生噪声，不代表上游调用失败 |
