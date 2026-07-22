# 发布记录

Role: 保存最终发布门禁、版本决策、提交、tag、推送、回滚点和发布后观察结果

Status: 当前候选已复测；发布被 build inventory / live repo target 阻断

在所有适用门禁通过前，本目录不会记录“可发布”。发布时必须先同步远端分支与 tag，使用项目版本权威计算新版本，工作修复提交与版本提交分开，先推分支再推 tag，不修改依赖要求版本，不 force push。

## 2026-07-22 发布准备状态

当前 release model 预计为 Rust crate：根 `Cargo.toml` `[package].version` 是项目版本权威。只读远端 tag 检查显示最新 semver tag 为 `v0.0.112`；当前根版本仍为 `0.0.109`。若 inventory 解除后继续 patch release，下一版应从远端最新 tag 推导为：

- next version: `0.0.113`
- next tag: `v0.0.113`

当前明确阻断不是当前候选测试失败，而是发布产物清单：

- `node feature/tests/inventory-build-artifacts.mjs --gate` 返回 `targets=1`、`target_processes=1`、`blockers=2`；
- live PID `84264` 以 `./target/release/kiro-rs -c config.json --credentials credentials.json` 运行；
- 该进程监听 `127.0.0.1:9022`，并引用仓库根 `target/release/kiro-rs` 与 `target/local-verify/kiro-rs-9022.log`；
- 因此不能删除 repo `target/`，也不能在未获明确授权时停止该 live 服务。

解除阻断后发布前最小步骤：

1. 停止/迁移 PID 84264，使 live 服务不再引用 repo `target/`。
2. 删除 repo `target/` 中的可再生产物并重跑 `node feature/tests/inventory-build-artifacts.mjs --gate`，必须 pass。
3. 重新跑 `node feature/tests/check-feature-docs.mjs` 与 `git diff --check`。
4. `git fetch origin --prune --tags`，重新计算远端最新 semver tag，确认仍为 `v0.0.112` 或按最新远端重新推导。
5. 提交工作修复；如需要版本 bump，则单独将根 `Cargo.toml` 版本更新到 next version 并提交 release bump。
6. 创建 annotated tag，先推 branch，再推 tag。
