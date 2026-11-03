# 发布与远端升级计划（0.0.226）

顺序：本机测试全部通过 → 合并 main、打 tag 发布镜像 → 镜像发布成功 → 升级远端 59137。前一步没完成，不做后一步。

## 1. 本机测试（发布前必须全部通过）

| 项 | 要求 | 状态 |
| --- | --- | --- |
| 容量压测 | 100 个 mock 账号，每号并发 3、RPM 20；60 并发起逐步升到 300；有排队或拒绝时不能有账号并发和 RPM 都没满 | 进行中 |
| 复现诱因 | 上游变慢、长流、部分账号慢、粘性会话集中、优先级分档、锁竞争 | 进行中 |
| 回归场景 | 已有 mock 场景（并发、报错、恢复、排队、协议、外部池）全部重跑 | 待做 |
| 全量测试 | 默认特性和无默认特性两套，CI 同一脚本，带 PgSQL、Redis | 待做（合并 main 后要重跑） |
| 真实账号 | 127 个可用本地账号：协议全集 + 64 并发 | 待做 |

## 2. 发布

- 版本号：main 上最新是 v0.0.225，本次用 v0.0.226（发布前再查一次 main 和远端 tag，有人先发就顺延）。
- 步骤：测试分支合并进本地 main（先快进到 origin/main）→ 改 Cargo.toml / Cargo.lock 版本 → 提交 → 推送 main → 打注解 tag `v0.0.226` 并推送。
- 监控：Quality Gate（main push）和 Docker 构建（tag）两条 Actions 都要成功，镜像 `ghcr.io/2ue/kiro-rs:0.0.226` 和 `latest` 可拉取才算发布完成。
- 失败：找原因、修复提交到 main、删除远端 tag、在修复提交上重建同名 tag 再推送，版本号不变。

## 3. 远端升级（发布成功后才做）

现状（2026-10-31 只读查询）：

- 59137：`~/docker-compose/kiro-rs-2ue-59137`，容器 `kiro-rs-2ue-59137-app`，镜像 `ghcr.io/2ue/kiro-rs:latest`，当前 0.0.225（`12e2db68`，镜像 `sha256:619593db…`）。同目录 PG、Redis 不动。
- sub2api：`~/docker-compose/sub2api`，容器 `sub2api`，库 `sub2api-postgres`。
- 59137 接入 sub2api 的账号：`accounts.credentials->>'base_url'` 以 `http://152.53.243.159:59137` 开头，未删除。当前 schedulable=true 的 20 个：

  2128 hc005-kiro、2129 hc005-kiro-cc、2130 hc005-kiro-ha、2131 hc005-kiro-na、2140 hc005-kiro-rst、2160 hc005-kiro-onlylocal、2202 hc005-kiro-minea、2203 hc005-kiro-mineb、2214 hc005-kiro-onlylocal-按次、2274 hc005-kiro-minerst、2275 hc005-kiro-minewdpd、2286 hc005-kiro-mineanci、2303 hc005-kiro-minesushua、2309 hc005-kiro-minehigh-huaqiangbei、2310 hc005-kiro-mine-ha173、2311 hc005-kiro-mine-haa copy、2316 hc005-kiro-mine-haa、2319 hc005-kiro-df-cc、2320 hc005-kiro-df-mcc、2321 hc005-kiro-df-na

  schedulable=false 的 2163、2164、2166、2241、2304 本来就停用，不动；2171–2181 是 152.53.242.178 的另一台，不在本次范围。
- 升级前再查一次：清单以升级时刻的查询结果为准，记下每个账号的 schedulable、temp_unschedulable_until、rate_limit_reset_at、overload_until。

还要确认（升级前做，只读）：

- sub2api 把账号设为临时不可调度时写哪些字段（`temp_unschedulable_until/reason`、`rate_limit_reset_at`、`overload_until`、Redis 里的缓存），以及恢复的正确方式：优先用 sub2api 的管理接口清除；没有接口再改库，并清 sub2api 的 Redis 缓存。先在一个本来就停用的账号上验证恢复方式，不在生产账号上试。
- 59137 新版本启动迁移是否改 runtime_config（本次迁移版本仍是 13，不应改动）。

升级步骤：

1. 预拉镜像：`docker compose pull -q kiro-rs`，核对镜像版本标签是 0.0.226、revision 是发布提交。这一步不影响服务。
2. 准备好恢复脚本（第 4 步要用）和回滚命令，放在远端，参数先填好。
3. 重建应用容器：`docker compose up -d --no-deps --pull never kiro-rs`（`nohup setsid` 后台跑），只重建 app，PG、Redis 不动。实测中断约 12 秒。
4. 应用容器一起来（`/readyz` 返回 ready）就立刻恢复 sub2api 里第 0 步记下的账号状态，不等流量验证。只清本次升级造成的临时不可调度，原来就停用的不碰。
5. 观察：`/readyz`、59137 日志、`usage_records` 新记录、sub2api 的成功率与报错、几个主要路由（/cc、/na、/dfcache/*）都有成功请求。观察 15 分钟没问题就算发布完成。

回滚（发现问题时马上做）：

1. 记录问题现象、request id、时间。
2. 把 `.env` 的 `KIRO_RS_VERSION` 临时改为 `0.0.225`（或直接用镜像 `sha256:619593db…`），`docker compose up -d --no-deps --pull never kiro-rs`。
3. 回滚同样会造成临时不可调度，起来后立刻按第 4 步恢复账号状态。
4. 回滚后把 `.env` 恢复成 `latest`（用户要求不写死版本）前，先确认 latest 不会再拉到有问题的版本。
