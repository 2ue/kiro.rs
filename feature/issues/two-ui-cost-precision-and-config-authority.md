# Two-UI Cost Precision And Config Authority

Status: `implementation-complete / formatter-and-build-passed / browser-pending`

Severity: P1

## 问题、现象与影响

发布包含 `ui` 与 `admin-ui` 两套嵌入前端。选定显示合同为：请求明细、billing drill-down 和 CSV 费用固定 8 位；小于 1 美元的汇总显示 6 位；绝对值大于等于 1 美元的汇总显示 2 位；明确的 overview/chart compact 指标可使用固定 2 位。非法数值和 null 不显示伪造费用。

复核发现两项实际不一致：旧汇总 formatter 用 `value >= 1` 而不是绝对值判断，`-2` 会被显示成 6 位；两套 CSV 直接写 JavaScript number，极小值可能用科学计数法，和详情 8 位合同不同。当前工作树已将每套 UI 收敛到本地单一 formatter 权威，修正负数，并让两个 CSV 费用列固定输出 8 位无货币符号数值。

两 UI 还会在保存 prompt 配置时把 prompt 子开关镜像到独立 `bodyConversion.*` 字段，造成后端 API 设置被下一次 UI 保存静默覆盖。

## 根因

费用展示曾由各页面各自调用 `toFixed` 或直接序列化 number，没有每种 UI surface 的单一精度合同；汇总用正向 `value >= 1` 判断而不是绝对值。配置侧则把 prompt 子开关和 body conversion 视为同一表单状态，在 load/save normalization 中双向镜像，破坏后端独立字段权威。

## 复现

对两套 UI 分别输入值矩阵并覆盖列表、详情、tooltip、CSV 和汇总；对配置先通过 API 写入互相不同的 promptSteering/bodyConversion 值，再依次执行 `ui save -> refresh -> admin-ui save -> refresh`，比较完整 JSON field diff。三个 viewport 均需检查文本溢出和交互，不能用 production build 代替浏览器结果。

## 值与页面矩阵

测试 `0`、`0.0000000049`、`0.000000005`、`0.00000123`、`0.99999999`、`1`、负差额、NaN/null；覆盖列表、详情、dashboard、tooltip、CSV、外部池汇总。桌面/窄桌面/移动三个 viewport，两套 UI 均执行。

## 方案

- 建立共享的展示合同和各前端本地单一 formatter 源；明细固定 8 位，汇总精度由明确决策控制，不能页面各自实现。
- promptSteering 与 bodyConversion 独立 round-trip；只修改用户实际编辑的字段。
- UI 文案将 operator prompt 与协议转换分区，不再称一个开关为所有优化的“总开关”。

## 验收

formatter 单测、API round-trip、两套 production build、浏览器截图与交互均通过；最长值不溢出，CSV 与页面使用同一数值合同。

## 当前验证结果

- `node feature/tests/cost-format-contract.mjs`：PASS。脚本使用仓库现有 TypeScript 编译器加载两套 UI 的真实 formatter，实现相同值矩阵；覆盖 `0`、`0.0000000049`、`0.000000005`、`0.00000123`、`0.99999999`、`1`、`-2`、NaN 和 null。
- 第一次尝试使用当前 Node 不支持的 `--experimental-strip-types` 失败，没有计为 pass；最终脚本不新增依赖，编译到一次性系统临时目录并自动清理。
- `ui npm run build`：PASS，2456 modules。
- `admin-ui npm run build`：PASS，1775 modules。
- promptSteering 保存路径不再把 prompt 子开关镜像覆盖 `bodyConversion.*`；静态测试/构建已通过，真实后端 save-refresh 和两 UI 交叉 round-trip 仍待浏览器 gate。
- 2026-07-16 再次按仓库规定的应用内 Browser gate 建立会话，仍在打开任何项目页面之前被宿主元数据缺失拒绝（固定分类：`sandbox-state metadata missing sandboxPolicy`）。因此本轮没有产生页面交互、viewport、截图、网络请求或 save-refresh 证据；该结果只证明 browser gate 仍被宿主工具阻断，不能记为 UI pass，也不能用独立 Playwright/其他浏览器替代后宣称通过。
- 桌面、窄桌面、移动 viewport 的详情、tooltip、CSV 下载和配置交互尚未执行，因此本专题还不能标记 `verified-fixed`。

## 残余风险与回滚

浏览器 round-trip、CSV 实际下载、tooltip 与三个 viewport 仍未执行。回滚可以恢复单个 formatter 的旧显示，但不得恢复 prompt/body 字段镜像覆盖；任何精度调整都必须同时更新两套 UI 和可执行值矩阵，避免同一费用在页面与 CSV 中再次分叉。
