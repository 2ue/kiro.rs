# 项目分析：`Kiro-account-manager`

路径：`/Users/yuanfeijie/Desktop/procode/Kiro-account-manager`  
最新本地提交：`447adcd`，2026-06-11  
相关度：中

`Kiro-account-manager` 是桌面账号管理和本地反代结合的项目。它不适合直接内置到当前服务，但账号管理体验、注册/导入、自愈、prompt cache、账号代理分桶等思路可参考。

## 关键文件

| 文件 | 作用 |
| --- | --- |
| `Kiro-account-manager/src/main/proxy/accountPool.ts` | 多账号轮询、断路器、指数退避、概率重试 |
| `Kiro-account-manager/src/main/proxy/kiroApi.ts` | Kiro API 调用、endpoint、headers、profileArn、agent mode、proxy |
| `Kiro-account-manager/src/main/proxy/promptCacheTracker.ts` | prompt cache 模拟 |
| `Kiro-account-manager/src/main/proxy/types.ts` | Kiro payload/account 类型 |
| `Kiro-account-manager/src/preload/index.d.ts` | 前后端账号、proxy、注册、检查、模型接口 |
| `src/main/kiroAuthSync*` | profileArn / auth 同步 |

## 账号池

`accountPool.ts` 有几个策略：

- `round-robin`：每次成功后指针前进。
- `sticky`：一个账号成功就粘住，提升 prompt cache 命中。
- 单账号时绕过断路器，直接返回，让用户看到真实 API 错误。
- 多账号时跳过 suspended、quota exhausted、token expired、isAvailable=false。
- errorCount 指数退避。
- 冷却期内允许小概率 retry，模拟 half-open。
- quota exhausted 全部耗尽时返回 null，不再乱切。
- suspended 是长期封禁，需要人工清理。

当前项目已有更强的调度和冷却，不应照搬概率 retry。但可以学习：

- suspended 和 transient cooldown 的概念分开。
- 单账号调试时可以选择暴露更真实错误给管理员测试，但公共接口仍归一化。
- sticky 作为 cache 命中策略需要独立说明。

## Kiro API 调用

`kiroApi.ts` 有几个值得学习的点：

- 账号绑定 proxyUrl 优先于全局代理。
- K-Proxy、环境变量、系统代理依次 fallback。
- endpoint 列表包含 CodeWhisperer、AmazonQ、AmazonQCLI。
- User-Agent 中包含 Kiro IDE version、SDK version、OS、Node、machineId。
- agent mode 可配置 `vibe` / `spec`。
- profileArn 决策集中：
  - 真实 ARN 优先。
  - Enterprise/IdC 用区域化 fallback ARN。
  - Social 用固定 social ARN。
  - BuilderId 用 placeholder ARN。
- Enterprise 首次解析出真实 profileArn 后通过 callback 持久化回主进程和磁盘。

当前项目已经有账号代理资源和 profileArn 逻辑，但可以学习：

- profileArn 自愈回写路径要清楚。
- 账号代理分桶在 UI 上要解释为“账号出口”，不要让用户误以为启用代理就一定提升性能。
- agent mode 的配置要有协议测试，不能只是 UI 开关。

## Prompt cache tracker

`promptCacheTracker.ts` 与当前项目思路类似：

- tool/system/message flatten。
- SHA-256 累积 hash。
- explicit breakpoint。
- message-end implicit breakpoint。
- 1024/4096 min cacheable tokens。
- 85% max cache ratio。
- 每账号最多 200 cache entries。
- 60s prune interval。

当前项目应重点学习内存边界：

- `MAX_ENTRIES_PER_ACCOUNT`
- prune interval
- total entries 管理端可见

当前项目的 high-cache 功能更复杂，但内存边界必须更直观。

## UI/账号管理体验

它的 preload API 暴露了大量账号能力：

- batch refresh。
- batch check。
- Builder ID 登录。
- IAM SSO 登录。
- import from bearer token。
- get models。
- get subscriptions。
- set overage。
- proxy status / upstream reachable / RT。

当前项目是服务端管理后台，不需要内置桌面登录器。但可以学习页面能力：

- 账号检查要分批、显示进度。
- 账号状态要展示真实 plan：free/pro/pro+/power 等。
- 模型列表和订阅信息应该是账号详情的一部分。
- 代理测试要显示是否可达、RT、错误，但不要暗示“启动代理一定有用”。

这也对应之前 UI 反馈：账号卡片默认信息应该更多，账号标识要更明确。

## 比当前项目强的地方

- 桌面账号注册/导入/检查体验完整。
- profileArn 自愈回写链路直观。
- 账号代理分桶逻辑明确。
- prompt cache 有 per-account entry cap。
- 账号状态/订阅/模型/overage 管理能力丰富。

## 当前项目比它强的地方

- 当前项目服务端生产化能力更强。
- 当前项目 Redis/PgSQL、多实例、usage、外部池、错误归一化更完整。
- 当前项目调度并发/RPM/dispatch wait 更适合现网。
- 当前项目不应把桌面 MITM 和服务端主路径混在一起。

## 建议吸收方式

P0：

- 管理端账号详情补真实 plan、模型列表、profileArn region、代理状态。
- prompt cache tracker 增加 entry 上限和统计。

P1：

- 账号 batch check / batch refresh 做进度和并发上限。
- profileArn 自愈回写做明确 trace。

不建议：

- 不要把桌面登录/MITM/证书安装内置到现网服务。
- 不要在请求热路径使用概率 retry。
- 不要把 K-Proxy 这类桌面代理概念照搬到服务端。

