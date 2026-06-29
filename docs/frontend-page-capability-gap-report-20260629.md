# 前端页面能力缺失记录

日期：2026-06-29

范围：逐页对比 `/admin`、`/console`、`/ui` 三套前端。新版 `/ui` 作为当前能力基准；旧版和次新版的布局可以不同，但同一业务能力不能缺失。本文只记录本次需要补齐的功能缺口，不评价视觉风格。

## 路由与页面基准

`/ui` 当前页面：总览、账号、校验、外部账号、代理、用量、审计、运行配置、模型、安全。

`/console` 当前页面：总览、账号、校验、代理、外部账号、用量、价格、审计、配置。

`/admin` 当前页面：总览、账号、校验、代理、外部账号、用量、价格、审计、配置。

模型和安全能力在旧版中分散到价格/配置/访问密钥等区域，不要求页面名称完全一致，但能力需要能找到。

## 配置页

基准能力：

- 调度：支持 `priority`、`balanced`、`health_balanced`、`weighted_least_inflight`。
- 上游超时：支持普通响应超时和流式空闲超时。
- 调度失败诊断：支持选择失败采样上限和是否记录诊断。
- 高缓存：支持全局高缓存模拟参数、缓存创建频次控制、真实 cachePoint 开关、缓存条目边界。
- 路径级缓存策略：支持按路径覆盖 `simulation`、`creationControl`、`reportedUsage`、`cachePoint`、`bounds`，最长前缀匹配。
- 自定义 `/dfcache/*` 路由：只允许配置名称，不允许修改 `/dfcache/` 前缀。
- 兼容：支持 thinking 触发策略、thinking 提取、模型解析、模型映射、Kiro 工作模式、代理告警。

`/admin` 缺口：

- 负载均衡下拉缺少 `weighted_least_inflight`。
- `RuntimeConfig` 类型缺少流式空闲超时、调度失败诊断、cachePoint、thinkingTriggerMode、缓存条目边界等字段。
- 配置保存时没有归一化上述字段，容易导致旧页面保存后丢失或回退新字段。
- 页面只实现了全局高缓存和路径级 usage 展示规则，没有完整的路径级 `cachePolicy` 编辑能力。
- 兼容区域缺少 thinking 触发策略。
- cachePoint 开关和缓存条目边界没有独立入口。

`/console` 缺口：

- 负载均衡下拉缺少 `weighted_least_inflight`。
- `RuntimeConfig` 类型和默认值缺少流式空闲超时、调度失败诊断、cachePoint、thinkingTriggerMode、缓存条目边界等字段。
- 配置保存时没有归一化上述字段。
- 页面只实现了全局高缓存和路径级 usage 展示规则，没有完整的路径级 `cachePolicy` 编辑能力。
- 兼容区域缺少 thinking 触发策略。
- cachePoint 开关和缓存条目边界没有独立入口。

本次补齐：

- 两套旧页面补齐类型、默认值、保存归一化和负载均衡选项。
- 两套旧配置页增加路径级 `cachePolicy` JSON 编辑器，覆盖高缓存模拟、缓存创建频次、usage 展示、cachePoint、缓存边界。
- 两套旧配置页补齐流式空闲超时、调度失败诊断、cachePoint、缓存边界、thinking 触发策略。

## 账号页

基准能力：

- 支持普通账号、IDC、API Key、external_idp 导入。
- external_idp 不要求 clientSecret；需要保留 accessToken、expiresAt、clientId、profileArn、region、tokenEndpoint、issuerUrl、scopes。
- 搜索/筛选支持状态、当前使用、冷却、限频、错误、自定义调度、自定义优先级、自定义并发、自定义 RPM、external_idp 等。
- 批量操作支持启停、删除、刷新、重置优先级、清除自定义并发、清除自定义 RPM、批量编辑、查询额度。
- 批量编辑支持优先级、并发、RPM、代理、区域。

`/admin` 缺口：

- 状态筛选缺少当前使用、冷却、限频、自定义调度、自定义优先级、自定义并发、自定义 RPM、external_idp。
- 批量操作缺少重置优先级、清除自定义并发、清除自定义 RPM、查询额度。
- 批量编辑缺少优先级。
- 批量 JSON 导入和 KAM 导入会把 external_idp 当成 idc，导致无 clientSecret 的 SSO 账号无法导入。

`/console` 缺口：

- 状态筛选缺少自定义调度、自定义优先级、自定义并发、自定义 RPM、external_idp。
- 批量操作缺少重置优先级、清除自定义并发、清除自定义 RPM、查询额度。
- 批量编辑缺少优先级和 RPM。
- 批量 JSON 导入和 KAM 导入会把 external_idp 当成 idc，导致无 clientSecret 的 SSO 账号无法导入。

本次补齐：

- 修正两套旧页面的 external_idp 导入透传和校验逻辑。
- 补齐状态筛选项。
- 补齐批量快捷操作。
- 补齐批量编辑字段。

## 校验页

基准能力：

- 支持输入 access token 批量校验。
- 支持上传账号 JSON/KAM 文件导入后校验。
- 支持测试模型、超时、并发、是否包含禁用账号等选项。
- 支持校验普通账号和 external_idp 账号。

`/admin` 缺口：

- 只支持粘贴 token，缺少文件导入入口和部分选项。

`/console` 缺口：

- 文件导入和选项已基本具备，重点确认 external_idp 字段透传不被导入层破坏。

本次补齐：

- `/admin` 校验页增加文件导入与校验选项。
- 两套旧页面复用修正后的导入解析，保证 external_idp 可进入校验流程。

## 用量页

基准能力：

- 支持搜索、模型、会话、状态、来源、路由目标、流式、最小缓存读取、endpoint 等筛选。
- 支持批量清理和清空全部。
- 错误详情展示保留内部诊断信息，但对下游暴露信息要归一化。

`/admin` 缺口：

- 缺少 endpoint 筛选。
- 清理弹层缺少清空全部入口。

`/console` 缺口：

- endpoint 筛选已具备。
- 清理弹层缺少清空全部入口。

本次补齐：

- `/admin` 增加 endpoint 筛选。
- `/admin` 和 `/console` 增加清空全部入口。

## 审计页

基准能力：

- 支持按关键词、成功/失败、操作分类筛选。
- 操作名称有可读标签，方便定位变更类型。

`/admin` 缺口：

- 只有列表和详情，缺少客户端筛选和操作标签归类。

`/console` 缺口：

- 只有列表和详情，缺少客户端筛选和操作标签归类。

本次补齐：

- 两套旧审计页增加关键词、状态、分类筛选。
- 增加常见操作的可读标签与分类。

## 代理、外部账号、价格、模型、安全

当前结论：

- 核心 CRUD 能力基本存在。
- `/ui` 将模型和安全拆成独立页面，旧页面以价格页、配置页、访问密钥区域承载，属于页面组织差异。
- 本轮不做额外拆页，避免引入导航和权限上的无关变化。

## 验证要求

本轮改动完成后至少执行：

- `admin-ui` 构建或类型检查。
- `admin-ui-daisy` 构建或类型检查。
- 对涉及的导入逻辑进行静态链路检查，确认 external_idp 不再要求 clientSecret。

