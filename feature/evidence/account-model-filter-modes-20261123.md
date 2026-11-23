# 账号模型筛选及防调度风暴验证

Date: 2026-11-23

Status: Released / all three image manifests verified

## 实施契约

同一账号的白名单与黑名单互斥，通过 `modelFilterMode: allowlist | denylist` 解释同一份 `supportedModels`。缺省白名单保持旧账号行为；白名单非空只允许所列模型；main/forced 本地账号空白名单继承订阅等级；外部与 Claude 账号空白名单不限制。黑名单只禁止所列模型，空黑名单不限制，也不继承本地订阅等级白名单。

单账号编辑支持模型下拉、手动输入和一键填充。本地账号与外部池均支持批量设置；Claude 保留替换、追加、移除、清空，跨模式须替换或清空。候选以系统模型目录为主，补充已有选项及单账号发现结果；黑名单下发现模型仅补充候选，不自动把可用模型写成禁止项。Claude 黑名单不能提升为模型目录。

## 防调度风暴

- 模型资格过滤先于租约和容量获取。无候选不是容量暂满，不进入容量重试等待。
- 请求模型、映射候选、账号映射目标任一命中黑名单都会拒绝；全局显式映射同样受限。黑名单不能作为“已知支持”目录来提升优先级。
- 模型变体按名单内容缓存，重复名单复用，每线程最多 128 组；编辑名单按新内容展开，模式解释不缓存，避免过期语义。
- main/forced 本地批量模型设置在批次结束合并为一次调度状态通知和配置发布。外部与 Claude 前端批量同时最多更新 4 个账号。
- 三分支并发 1/16/64 拒绝场景均通过：零 Redis 容量调用，热缓存不重复查 PostgreSQL，无候选且没有临时容量等待条件；更新名单后 64 并发共享一次配置加载。
- 同名单 1000 次复用、缓存上限、编辑后失效、本地批次唤醒合并、支持账号满载/被排除、订阅等级覆盖、存储迁移/复制/重载均有回归。

## 精确源码及全量门禁

表中的 Rust 结果为每种特性配置各自的通过/失败数量；默认和无默认特性均通过。main/forced 既有 18 项 ignored、Claude 既有 12 项 ignored 保持原状态。独立时间敏感门禁也全部通过（main/forced 各 10 项，Claude 8 项）。前端构建、Rust 格式、Clippy 基线及发布构建通过。

| 分支 | 发布标签 | 源码 | Rust 通过/失败 | UI 通过/失败 | 验证 |
| --- | --- | --- | --- | --- | --- |
| main | `v0.0.264` | `ce7bdea9` | 3193 / 0 | 91 / 0 | [全量检查](https://github.com/2ue/kiro.rs/actions/runs/45116005616) / [镜像发布](https://github.com/2ue/kiro.rs/actions/runs/45118286275) |
| forced-main | `forced-v0.0.273` | `57db0f86` | 3356 / 0 | 91 / 0 | [全量检查](https://github.com/2ue/kiro.rs/actions/runs/45116114876) / [镜像发布](https://github.com/2ue/kiro.rs/actions/runs/45118326461) |
| claude-main | `claude-v1.0.17` | `7854cbe4` | 1255 / 0 | 177 / 0 | [全量检查](https://github.com/2ue/kiro.rs/actions/runs/45118781511) / [镜像发布](https://github.com/2ue/kiro.rs/actions/runs/45119962759) |

main 按正常历史合入 forced-main，保留 forced 的定价与模型目录差异；Claude 独立移植。最终 Claude 源码同时包含并行任务的模型点号兼容移除，已按固定提交重新完成全量验证。

## 镜像远端清单

以下清单通过远端 registry 直接读取，均包含 linux/amd64 与 linux/arm64。

| 镜像 | 平台 | 子清单 digest |
| --- | --- | --- |
| `ghcr.io/2ue/kiro-rs:0.0.264` | linux/arm64 | `sha256:a7948ae83331c2a7f26777f6add31e1d650bf3f085615e6f3ceaecd845b81878` |
| `ghcr.io/2ue/kiro-rs:0.0.264` | linux/amd64 | `sha256:ed6fc27b215cbace0f38ecd16d7029027e3de04f54d8a6c25cf2776d72a24b4f` |
| `ghcr.io/2ue/kiro-rs-forced:0.0.273` | linux/amd64 | `sha256:4098cc56170a7e64061f625afd038b9fec973266d0683d6b8759eef0c83a8925` |
| `ghcr.io/2ue/kiro-rs-forced:0.0.273` | linux/arm64 | `sha256:41650dfea04064cdfa97d46b9f3a80ad52e72f1d95fbb112cf909ab13ecb29db` |
| `ghcr.io/2ue/claude-pool:1.0.17` | linux/arm64 | `sha256:8f7b019247c7e995b0dda760ed5bc503ba6e9b47db6701ba6b0f4d6ad2f49a53` |
| `ghcr.io/2ue/claude-pool:1.0.17` | linux/amd64 | `sha256:9ba4dfbff3f165dcf0bc1435e13cb637922025629e2461cea94b6ad06bf32834` |

## 发布修复与清理

Claude 首次创建的 `claude-v1.0.17` 指向并行任务刚提交的 `33f5907b`，格式门禁失败后已取消发布，未发布版本 manifest。修复后的 `7854cbe4` 全量门禁通过后，仅重建了本轮失败标签，固定到该提交，再完成镜像发布；main 与 forced 标签保持原提交。

本轮所有作用域构建 target/reservation 已清理，最终发布 inventory 为零 target、零 reservation、零引用进程、零 blocker。三个临时测试数据库已删除；界面验证的临时 Vite、页面和测试二进制已清理。既有 `ui/node_modules` 链接保留。未执行生产部署。

## 日志摘要哈希

完整 CI 日志可从上述 Actions 检索；本地原始日志仅作为临时验证材料。以下 SHA-256 用于对应本轮取得的完整 CI 输出：

- main: `4f21edeeeff86da7a2ed12039cd0f41feaa54aed651ab408104276125661f6d8`
- forced: `96a6ee94923c84bf2e4aac1eb8befcd55c1e37b017b5a98820ab351b2ca0fb90`
- claude: `c2fa2d61ea2517acdc29e384da127ec436ea016db4aebe898d662e4791940082`
