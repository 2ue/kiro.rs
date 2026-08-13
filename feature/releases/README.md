# 发布记录

Role: 保存最终发布门禁、版本决策、提交、tag、推送、回滚点和发布后观察结果

Status: `v0.0.131` 已发布；post-release observation remains open

在所有适用门禁通过前，本目录不会记录“可发布”。发布时必须先同步远端分支与 tag，使用项目版本权威计算新版本，工作修复提交与版本提交分开，先推分支再推 tag，不修改依赖要求版本，不 force push。

## 2026-07-23 发布准备状态

当前 release model 为 Rust crate：根 `Cargo.toml` `[package].version` 是项目版本权威。2026-07-23 发布前 `git fetch origin --prune --tags` 后，远端最新 semver tag 为 `v0.0.113`；当前根版本在工作提交后仍为 `0.0.109`，因此下一版从远端最新 tag 推导为：

- next version: `0.0.114`
- next tag: `v0.0.114`

当前明确阻断已解除。最终门禁：

- Work commit: `b528ead` (`fix: harden runtime protocol and scheduler gates`)。
- Final frozen `kiro-rs` SHA-256: `925525419cd48b460217df2568891a40287da0c44d2bf921a38b103c047775ee`。
- Final frozen `kiro_loadtest` SHA-256: `90babda7388aa93854cbbdb81c132cc436c07f46b0ea22973531b0a7ffb3aff1`。
- Rust C0/release: `cargo +1.92.0 fmt --all -- --check`、`cargo +1.92.0 test --all-targets`（main `1750/0/6`，`kiro_loadtest 31/31`）和 `cargo +1.92.0 build --release --bins` 通过。
- Non-Cargo gates: feature docs 47 issue docs / 115 links、Node contracts `283 tests / 261 pass / 22 explicit skips / 0 fail`、`git diff --check` 均通过。
- Build inventory: 删除可再生 repo `target/` 后最终 `targets=0 reservations=0 target_processes=0 blockers=0`。
- Docker 动态验证按用户要求豁免，不记为 pass；既有 `127.0.0.1:9022` 未停止、重启或压测。

发布步骤与结果：

1. 将根 `Cargo.toml` 和 `Cargo.lock` 当前 crate 版本更新到 `0.0.114`。
2. 提交 release bump：`beb9b3420b20776db489461d65392b5b1d6e5d92` (`chore(release): 0.0.114`)。
3. 二次 fetch 远端 tags，确认 `v0.0.114` 不存在且远端最新仍是 `v0.0.113`。
4. 创建 annotated tag `v0.0.114`。
   - tag object: `071ccb3975fb1ae2bf6cd27f9875f9dd4b9a24e8`
   - peeled commit: `beb9b3420b20776db489461d65392b5b1d6e5d92`
5. 先推 `main`：`401473c..beb9b34  HEAD -> main`。
6. 再推 `v0.0.114`：`[new tag] v0.0.114 -> v0.0.114`。

发布模型：`rust-crate`。

版本权威：根 `Cargo.toml [package].version = "0.0.114"`，与 tag `v0.0.114` 的数字部分一致。

远端验证：

```text
git ls-remote --tags --refs origin v0.0.114
071ccb3975fb1ae2bf6cd27f9875f9dd4b9a24e8 refs/tags/v0.0.114

git rev-parse v0.0.114^{}
beb9b3420b20776db489461d65392b5b1d6e5d92
```

Post-release note: 本段是在 tag 推送成功后补写的文档记录；不会移动已发布 tag，也不会修改 `v0.0.114` 指向的 release commit。

## 2026-08-03 v0.0.131 失败恢复后重新发布

本次使用 `tag-only` release model。`Cargo.toml` 根 crate 版本已经是
`0.0.131`，因此没有新增版本 bump commit；修复提交单独提交并先推送
`main`，再重建并推送同名失败 tag。

- 修复提交：`511cebb60e26d970b77b33a3638ec8d9806505de`
  (`fix: raise usage cleanup safety limit`)。
- 旧失败 tag：删除远端和本地 `v0.0.131`，因为用户明确要求重新发布 131。
- 新 annotated tag：`v0.0.131`，peeled commit 为 `511cebb60e26d970b77b33a3638ec8d9806505de`。
- 远端 Docker workflow：`Publish Docker Images #162`
  (`30800052601`)。
- 结果：`Success`，总时长 `25m 36s`；quality、amd64/arm64 build 和
  multi-architecture manifest 全部成功。

远端校验：

```text
git ls-remote origin 'refs/tags/v0.0.131^{}'
511cebb60e26d970b77b33a3638ec8d9806505de refs/tags/v0.0.131^{}
```

本次发布修复了首次 `30757990049` 因 `src/model/config.rs` Clippy
bucket 回归导致的失败。生产服务未被本地操作；部署是否已拉取新镜像仍需
按现网发布流程单独观察。
