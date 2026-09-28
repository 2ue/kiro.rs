# 导入账号标签属性

Status: `Done`

Last reviewed: 2026-09-28 Asia/Shanghai

## 目标

在账号导入弹层中支持为新账号选择已有标签或输入新标签，并把标签作为账号
自身的属性保存到现有 credentials JSON/PgSQL 记录中。此功能不建立独立标签表、
不引入标签实体生命周期，也不改变账号去重或调度语义。

## 范围

In scope:

- `KiroCredentials` 增加可向后兼容的 `tags: Vec<String>` 字段；
- Admin `AddCredentialRequest`、批量导入默认值、导出和账号列表响应传递标签；
- 主 UI 与 Admin UI 的单条新增、批量导入、文件填充/KAM 导入弹层支持标签；
- 已有标签从当前账号列表动态去重、排序并作为候选；
- 标签输入规范化：去除首尾空白、忽略空值、去重、限制单标签和总数量长度；
- Rust round-trip/导入默认值/API 响应与前端类型、构建回归。

Out of scope:

- 新建 `tags` 表、标签 ID、标签权限或独立 CRUD；
- 标签筛选、批量编辑和独立标签管理页；
- 修改生产机器、生产数据库或生产配置；
- 改变账号认证、调度、刷新、去重或凭据存储主键语义。

## 设计决策

1. 使用 `Vec<String>` 而不是单个字符串，以支持一次导入选择多个已有标签并保留
   JSON 导入/导出能力。
2. `tags` 缺失时反序列化为空列表；空列表不写入 JSON，兼容所有历史账号。
3. 后端对标签做最终规范化，不能依赖浏览器；导入接口和文件导入都走同一规则。
4. 候选标签不单独查询数据库，直接由已有凭据列表响应的 `tags` 字段计算，避免
   增加新的存储边界和同步问题。
5. 标签仅用于展示和后续筛选扩展，永远不进入 refresh、credential hash、machine
   identity、调度选择或上游请求。

## 规范化约束

- 单标签首尾空白会被移除；
- 空标签丢弃；
- 同一账号内大小写敏感去重并保持首次出现顺序；
- 单标签最多 64 个 Unicode scalar；
- 单账号最多 32 个标签；
- 总标签字符数最多 1024；超限由后端返回可读的 invalid request；
- 列表候选按 Unicode 字符串稳定排序，前端输入中新标签可直接回车/点击添加。

## 验收矩阵

1. 历史无 `tags` 字段的账号可正常加载，序列化不产生空字段。
2. camelCase 与 snake_case 导入 JSON 均能读取 `tags`，导出后可重新导入。
3. 单条新增、批量导入、KAM 文件填充都把规范化后的标签写入账号属性。
4. 已有标签在两套导入弹层中列出，点击可选/取消；新标签可输入并与候选合并。
5. 空白、重复、超长、超数量标签行为在后端和 UI 都有测试或明确提示。
6. 账号列表/分页/完整状态响应带出标签，前端不会因旧响应缺失字段崩溃。
7. Rust 全量测试、前端 typecheck/build、diff 与 artifact inventory 通过。

## 风险与回滚

- 主要风险是只改 UI 未传到持久化层，或只写单条导入而漏掉批量/KAM 路径；
  因此验收必须覆盖所有入口和导出回读。
- 后端规范化是最终边界，避免脚本或旧客户端写入不可控标签数据。
- 回滚只需回退二进制/关闭新入口；已有 `tags` 字段会被旧版本忽略，不删除账号。

## 进度

- [x] 现状盘点与存储边界确认
- [x] 计划与验收矩阵记录
- [x] Rust 模型、Admin API、导入/导出链路
- [x] 主 UI 与 Admin UI 标签选择器
- [x] Rust/前端/真实本地实例回归
- [x] 合并到功能提交并重新发布 `v0.0.174`

## 已验证证据

- Rust 全量回归：主二进制 `2196 passed / 0 failed / 6 ignored`；
  `kiro_loadtest 31 passed / 0 failed`。
- 标签模型、历史兼容、边界规范化、调度配置不变性及 Admin 默认标签定向测试通过。
- 主 UI `pnpm check`、主 UI `pnpm build`、Admin UI `pnpm build` 通过；
  `cargo fmt --check` 与 `git diff --check` 通过。
## 发布证据

- 功能提交：`05bbbf3`（`feat: add credential import tags`）。
- 发版提交：`83fd06b`（`chore(release): 0.0.174`）。
- 已删除旧远端/本地 `v0.0.174`；`v0.0.175` 在本地和远端均已不存在，因此没有执行
  对不存在标签的删除操作。
- 新 annotated `v0.0.174` 已推送，剥离后指向 `83fd06b`；远端 `main` 同样指向
  `83fd06b`，本地工作树与远端一致。
- GitHub Actions `Publish Docker Images #231`（run
  `36378663559`，2026-09-28）成功完成：`quality / Frontend and Rust quality gate`、
  `build (linux/amd64)`、`build (linux/arm64)` 和 `manifest` 全部通过。
- 本次发布未修改生产机器、生产数据库或生产配置。
