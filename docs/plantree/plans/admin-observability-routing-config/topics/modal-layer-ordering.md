# 弹窗、抽屉与二次确认的层级

Role: 当前实现约束与修复验证

Date: 2026-11-27

Status: 实现并完成本地验证，准备三条发布线

## 问题与修复

安全页修改登录 Key 使用全局 `ConfirmProvider`。旧版 `Dialog` 和 `Sheet` 把蒙层与内容作为两个 Portal 子项分别挂载，且都使用 `z-50`；绘制前后依赖实际挂载顺序。全局二次确认也与普通弹窗同层，无法明确压过已有弹窗和 `z-60` 下拉菜单。

共用 `ModalRoot` 透传受控/非受控打开状态；`ModalPortal` 把每个弹窗的蒙层和内容放入同一个隔离层叠容器，确保它们整体参与外层排序。普通容器为 50，二次确认为 80；容器内蒙层为 0，内容为 10。菜单/Popover 60、Tooltip 70 保持高于普通弹窗并低于全局确认。关闭状态由根组件传递给容器，退出动画结束后销毁，避免遗留全屏层。

普通弹窗、抽屉和嵌套弹窗按打开时的 Portal 挂载顺序整体叠放，后开的蒙层能覆盖前一个窗口，且不会盖住自己的内容。

## 同类排查

- 密码修改、请求 Key 删除及用量清零、开放号池确认。
- 用量清理弹窗内的二次确认。
- 账号和外部池删除、代理删除与解绑、路由策略/场景删除、审计自动清理和提示词保存确认。
- 账号抽屉内打开编辑弹窗及后续确认。
- 表单内 Select、Popover、DropdownMenu 与 Tooltip 的层级。

以上确认路径统一通过 `ConfirmProvider` 修复；普通窗口通过 `DialogContent`/`SheetContent` 使用同一层叠规则。

## 验证

- `pnpm --dir ui build`：通过。
- `node --experimental-strip-types --test ui/tests/*.test.ts`：91 项通过。
- 本地开发页面、React StrictMode、真实共用组件：普通弹窗、表单内确认、嵌套弹窗内确认、抽屉内弹窗与确认、取消/确认后父窗口恢复，共 11 项命中检查通过。用 `document.elementFromPoint` 验证窗口实际位于蒙层上方；Promise 确认/取消结果正确。
- Select 下拉显示在表单上方，Escape 关闭下拉后父表单保留；全部关闭后无残留层和页面指针锁定。
- 分支边界遵循 [发布基线](../../../baseline/test-and-release-gates.md#maintained-branch-boundaries)：main 修复后仅向 forced-main 合并；claude-main 独立移植共用组件修改。

发布结果以各分支 Git 标签及对应 Actions 发布任务为准。
