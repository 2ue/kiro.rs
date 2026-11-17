# 账号列表两层分类

Status: Implemented And Locally Verified / 2026-11-17

## 范围与规则

- 第一层：全部、启用、已禁用。第二层：全部、正常、额度用尽、账号异常、待确认；两层交叉筛选，每层内部互斥且覆盖全部账号。
- 正常表达账号健康，临时冷却、RPM 限流、并发满、观察期、代理阻断另作运行状态多选筛选；多选按同时满足处理。
- 切换第一层重置第二层；筛选变化回第一页、清除选中。Tab 计数遵循当前其他筛选条件；第一层排除两层 Tab 条件，第二层保留第一层并排除自身条件。
- 历史认证失败之后已成功调用的账号不继续判为当前认证异常；明确暂停/失效/锁定原因仍优先。残留健康计数不称为连续错误。
- 列表、统计、卡片、导出遵循同一分类规则。未知禁用原因进入待确认，手动禁用的健康账号仍属正常。
- 仅管理查询与 UI，不修改调度、Redis、数据库 schema 或历史记录。2026-11-17 后续已授权发布 main，并将 main 合并到 forced-main 后分别发布。
- 公开账号池保留原单层“正常”筛选口径，避免健康维度调整让禁用账号进入其正常列表。

## 验收

- 476 样本应分为启用 433 / 已禁用 43；账号健康为正常 433 / 额度用尽 41 / 账号异常 2。
- 历史错误恢复、认证持续失败、手动禁用、纯 RPM 限流和组合筛选有定向测试。
- 前端构建与浏览器验证覆盖两层切换、多选、清除、导出筛选和窄屏；Rust 验证走 scoped wrapper。
- 无历史回填、无线上写入。

## 验证

基于本地 `main` 的 `8946fe38`（`0.0.252`）工作区修改。以下验证均于 2026-11-17 执行。

- Rust `1.92.0`，通过 `feature/tests/run-cargo-scoped.sh account-classification-verified -- bash -c 'cargo fmt && cargo test --no-default-features admin:: -- --test-threads=2 && cargo fmt --check'`；设置 `KIRO_VALIDATION_RESERVE_KIB=6291456`、`CARGO_PROFILE_TEST_DEBUG=0`、`CARGO_PROFILE_DEV_DEBUG=0`，保留默认 20 GiB 空间底线。管理模块 **135 passed / 0 failed**，exit 0；包含分类互斥、恢复后的历史认证错误、AND 运行筛选、搜索、导出参数和公开账号池回归。没有运行真实 PgSQL/Redis 集成验证。
- `pnpm -C ui build`，exit 0；包含 TypeScript 检查。Vite 存在现有 bundle 大小提示。
- `node --test ui/tests/*.test.ts`，**80 passed / 0 failed**，exit 0。
- 浏览器只连接本机 Vite + mock API，使用 476 个代码生成的 `example.test` 虚拟账号；没有导入远端账号或调用上游。验证两层计数、禁用/额度分类、切第一层重置第二层、RPM + 并发满只剩 1 个账号、异常卡片、按 ID 搜索、空结果、清除和组合筛选导出。导出 URL 包含 `enabled=true&status=normal&runtimeStatus=concurrency_full,rate_limited`，mock 返回账号 #1。
- 桌面 `1440×1000` 与移动视口 `390×844` 均无横向溢出。没有发现页面运行错误；开发控制台有现有 React Router v7 future flag 提示。截图：[桌面](../../../../../tmp/account-classification/account-tabs-desktop.png)、[移动端](../../../../../tmp/account-classification/account-tabs-mobile.png)。截图为本地临时验证产物。
- `git diff --check`，exit 0；`node feature/tests/inventory-build-artifacts.mjs --gate`，exit 0，`targets=0 reservations=0 target_processes=0 blockers=0`。本次 scoped target 和空间 reservation 已自动释放。
- 本次 mock API、Vite 进程和临时 mock 脚本已清理，端口已释放；保留脱敏截图和本地验证日志。

## 发布跟进

2026-11-17：按后续指令准备主线 `v0.0.253` 和 forced 分支 `forced-v0.0.259`；保留各分支版本和发布流程，forced 合并包含完整 main 历史。两条流水线的最终结果以 GitHub Actions 和镜像 manifest 为准。
