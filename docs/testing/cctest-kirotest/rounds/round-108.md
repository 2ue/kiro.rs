# Round 108：必要能力配置、强制映射修订与独立分支发布

本轮在 `forced-main` 上验证并发布，不合并 main。59138、59140 始终只读，业务请求只发往既有隔离测试实例 59141，经 `https://kirotest.okmcode.com/dfcache/ak47` 访问。复用 Round 107 从 59140 复制的业务配置、账号及外部池；在此基础上仅为 ak47 开启服务端代码执行和模型驱动 WebSearch。强制目标仍为 Haiku 4.5，请求模型 Opus 5.5，服务 usage 保持开启，CCTest token 审计关闭，并发 1。

## 配置

- `bodyConversion.codeExecution.pathOverrides["/dfcache/ak47"] = true`
- `routeFeatures.pathOverrides["/dfcache/ak47"].modelDrivenWebSearch = true`

`../config/ak47-capabilities-patch.json` 是两个字段的递归合并补丁。应用时应合并到现有配置，保留其他路由、默认值与开关；不能把其顶层片段直接整体替换运行配置。此次没有向 59138 或 59140 应用该补丁，发布镜像也不会自动变更数据库配置。

配置开启后的修订前基线为 50 分，工具 10、结构化 10、知识 10、文档 10、指纹 10，流结构 5；非流、WebSearch、签名、模型归因、图片为 0。完整结果 `../results/20261116T134914-round108-capabilities-baseline.json`，结果 ID `5aa50e71-e39d-45b3-80ba-76a722bb9aae`。

## 代码范围

1. 显示模型与实际目标不同时，身份卡不再把公开模型日期当作映射部署的实际训练截止日期；缺少可验证信息时明确说明无法可靠确定。
2. 强制映射的结构化格式合约要求不确定性、限制与解释也遵循 schema，在允许的描述字段中表达，不编造事实或扩展字段。
3. 强制映射请求不再把客户端整数输出要求重复提升到 system；保留原始请求限制。非强制仍使用原有整数提示逻辑。
4. 强制映射下模型驱动 WebSearch 的内部调用，按隐藏思考语义移除回答开头的兼容 XML reasoning，避免当作普通正文发出。先计算 usage，再隐藏文本，引用和计费 token 均保留。

上述行为修订都受强制映射路径限制。没有改图片字节处理，没有修改非强制模式的身份、结构化规划或整数约束行为。图片历史对照已经显示上游存在字符误读，本轮不将偶发图片满分视为稳定修复。

## 验证流程与环境边界

本地通过 scoped Cargo wrapper 执行格式、全量 bin 单元测试与 release 构建，单任务、nice 15、ionice 3、`opt-level=0`、`lto=false`、`debug=0`，每 5 秒采样内存、swap、负载及三个实例健康。该配置用于降低本机已有服务的资源压力；生产优化、两种 TLS feature 的完整测试、PostgreSQL/Redis 必需集成验证和最终打包由发布 Action 的 full quality gate 执行。本地未设置必需 storage 集成环境，部分测试自行跳过，不能把本地数量等同于 CI 的完整存储覆盖。

首次本地测试为 3225 passed、1 failed、18 ignored：新增 WebSearch fixture 使用没有换行或标点分隔的 XML，现有解析器按普通 XML 文字保留，因此隐藏断言失败。真实抓包的 reasoning 使用换行分隔。只将 fixture 修订为真实格式，未修改通用 XML 解析规则，然后重新执行完整构建批次。

同域名的完整 CCTest 每轮独立启停抓包，关闭 token 审计但保留服务 usage。真实 CLI 使用隔离 HOME/config/work，原始 stdout/stderr 只保留哈希，临时目录用 finally 清理。强制关闭回归只修改 59141 的映射 enabled 字段，完成后恢复开启，不使用旧配置 restore 覆盖能力开关。

## 最终构建结果

第二次完整构建批次通过：**3226 passed、0 failed、18 ignored**，然后成功生成可运行二进制。SHA-256 为 `ee51b698c973983740b7189804253c4518054310b0970f72fdba2aa87c371ef5`。本地构建配置与 CI 覆盖范围的区别如上；不把此处的低优化构建当作生产镜像打包验证。

两次构建都完成 scoped target 和 reservation 清理。第二次采样 205 份，最低可用内存 5055.04 MiB、最低可用 swap 14243.95 MiB、最高一分钟负载 4.1343，没有触发暂停。59138、59140、59141 的采样健康检查均为 200。换二进制仅停止并重启已核验归属的 59141，生产端口监听未变。详见 `evidence/round108-build.json`。

## 新二进制的完整 CCTest

保持上述业务配置、Haiku 4.5 强制目标和 Opus 5.5 请求，连续完整执行两轮，均使用域名并独立抓包。结果原样存档，没有以单项重放替换完整测试分数。

| 分项 | 修订前且能力已开启 | 新二进制第一轮 | 新二进制第二轮 |
| --- | ---: | ---: | ---: |
| LLM 指纹 | 10 | 10 | 10 |
| 流结构 | 5 | 5 | 5 |
| 非流结构 | 0 | 0 | 0 |
| WebSearch | 0 | 0 | 0 |
| 签名 | 0 | 0 | 0 |
| 结构化输出 | 10 | 10 | 10 |
| 服务端工具 | 10 | 10 | 10 |
| 知识库 | 10 | 10 | 10 |
| 模型版本归因 | 0 | 0 | 0 |
| 文档识别 | 10 | 10 | 10 |
| 图片识别 | 0 | 0 | 10 |
| CCTest 百分制总分 | 50 | 50 | 59 |

表内分项和总分均直接采用 CCTest 返回值；分项与总分并非简单相加。图片在同配置、同二进制下仍有 0/10 波动，没有达到稳定满分。相对于 Round 107 完全对齐源配置时的工具 0，本轮开启 ak47 代码执行后工具连续为 10；代码修订前同配置基线已是 50，因此不能把这 50 分全部归功于本轮代码。

- 第一轮：[结果 c03471cc](https://cctest.ai/zh/result/c03471cc-f23b-4ef8-aaba-83d9c06eaa5a)，`results/20261116T142639-round108-release-258.json`，抓包 `capture-20261116T142639Z-205f3c55`。
- 第二轮：[结果 60113460](https://cctest.ai/zh/result/60113460-df32-48f4-b0af-92d846301e7f)，`results/20261116T142805-round108-release-258-confirm.json`，抓包 `capture-20261116T142805Z-f161e5ff`。

抓包证明 WebSearch 修订前下游正文含 `<thinking>`，修订后两轮均不含；仍保留 各 16 个引用 delta、1 次搜索请求、非零输入输出 usage，均正常 `end_turn`。该协议问题已修复，但 CCTest WebSearch 分项仍为 0，不能宣称该评分通过。

知识库两轮均明确表示无法可靠确定实际训练截止日期，并得到 10 分。归因有效取数由基线的 1 份变成两轮的 2/3 份，仍有拒答或数量/范围错误；CCTest 均判断为 Sonnet 4.6，归因分仍 0。这只是评分器统计判断，不作为实际切换服务模型的证据，实际强制配置仍为 Haiku 4.5。

图片客户端与上游 base64 哈希一致，上游最终文字与下游一致；第一轮三张图均正常完成但总项为 0，第二轮第一张即通过。没有修改图片处理链，也没有以重写识别结果获取分数。非流结构、签名与剩余 WebSearch 兼容差异继续保留，未伪造上游官方签名或不透明字段。脱敏抓包摘要见 `evidence/round108-packet-analysis.json`。

## 强制与非强制回归

强制和非强制两种模式的核心直接协议均 **8/8 通过**：文本、JSON、客户端工具的流/非流、adaptive 非流、真实可见 thinking 流。两种模式的客户端工具结果回传后生成 schema JSON 均通过；真实 Claude Code CLI 2.1.283 的普通文本与 Bash 工具往返共 4/4 通过，均有真实 thinking delta、非零输入输出 usage。仅临时关闭 59141 的强制开关执行非强制回归，最后恢复开启。

附加最小 PDF 转写第一次在两种模式均失败，都是 HTTP 200 后模型给出无法读取文档的解释。保留失败原始摘要，不将扩展矩阵标为零失败。强制模式随后对同一请求单独抓包重放两次，均正确返回 `cctest-isolated-doc-17`；抓包确认提取文本和既有 inline 文档说明均已送达上游。这说明同一构建能够完成提取与转写，模型回答仍有波动，不能用重放成功覆盖首次失败或认定全部文档稳定通过。CCTest 文档项两轮均为 10。

详见 `evidence/round108-direct-results.json`、`round108-direct-disabled-results.json`、`round108-tool-roundtrip*-results.json`、`round108-cli*-results.json` 和 `round108-pdf-probe.json`。CLI 原始 stdout/stderr 仅保留哈希，隔离 HOME/config/work 已删除。运行期间无负载或混沌压力，真实请求顺序执行；本轮没有调度逻辑修改。

## 最终配置及资源核对

只读重新提取 59140 配置，对比 59141：忽略 `port`、`postgres`、`redis`、`observabilityRedis` 四个隔离字段后，测试配置严格等于源配置加本轮批准的两个能力开关，业务哈希 `d836bfdcae0854102d59e6d3e691f2363203926031947780e681bccf03d2ba14`。强制开启、目标 Haiku 4.5、usage 开启均已确认，三个端口健康均为 200。详见 `evidence/round108-final-config-parity.json`。

两轮 CCTest 和回归的生产健康采样均为 200，运行测试期间最低可用内存约 9.8 GiB。59141 第二轮结束 RSS 约 211 MiB，回归结束约 218 MiB，FD 分别为 35/35；这些是低并发短时验证，不能替代长期 soak 结论。汇总见 `evidence/round108-runtime-resources.json`。

## 发布

本次版本只增加一次至 `0.0.258`，使用当前独立分支的 `forced-v0.0.258` 标签触发 `docker-build-forced.yaml`，不创建 main 发布标签。Action 先执行完整质量门禁，再发布 amd64/arm64 镜像与多架构 manifest。目标镜像 `ghcr.io/2ue/kiro-rs-forced:0.0.258` 和 `:latest`；最终 Action 状态另存 `evidence/round108-release-action.json`。

发布镜像不会自动把这两个数据库开关写入 59140。本轮只在 59141 启用配置，没有对 59138、59140 做部署、重启或写配置。
