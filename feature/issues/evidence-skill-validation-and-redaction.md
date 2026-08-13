# Evidence Skill Validation And Redaction

Status: `local-protection-validated-official-validator-unavailable`

Severity: P1 release gate

## 问题、现象与影响

生产证据 skill 和打包脚本已有历史使用记录，当前工作树又增加 email redaction。官方 `quick_validate.py` 在本机仍因 `ModuleNotFoundError: yaml` 失败，因此不能标记 quick validation 通过。

若 validator 依赖未声明的宿主包，现网取证流程无法从干净环境复现；若 archive 打包的 JSON key、URL credentials、symlink 或旧 archive 嵌套未正确处理，最终文档和诊断包可能泄漏敏感信息或产生不可重复 manifest。

## 根因

旧校验器将 PyYAML 当作隐式环境依赖；打包器又同时处理文本、JSON/JSONL、tar metadata、raw exclusion 和路径边界，但缺少一个包含所有假 secret 形态的固定 fixture。manifest 自引用与非确定 gzip/tar metadata 还会让相同输入产生不同 hash。

## 复现

在无 PyYAML 环境运行 official validator，确认 import 阶段失败；随后以只含假 API key、JWT、email、AWS key、PostgreSQL/Redis/HTTPS URL credentials、Authorization JSON、symlink 和旧 archive 的 fixture 连续打包 3 次。解包后扫描所有成员，要求 raw 不存在、secret 不存在、redaction marker 存在且固定 epoch hash 相同。

## 方案

- 提供项目内无第三方依赖的等价结构校验，或将 PyYAML 声明为可重复的开发依赖；不能依赖某台机器手工安装。
- 对 SKILL frontmatter、folder/name、agents metadata、references 和 scripts 做静态校验。
- 构造只含假 secret 的 evidence fixture，验证 API key、JWT、email、URL credentials、数据库 DSN 等全部 redacted，raw 默认不入包。
- forward test 只能使用本地假部署；真实生产审计继续遵守只读、bounded、先业务诊断后日志的规则。

## 验收

quick validation 可从干净环境一条命令通过；打包同一 fixture 3 次得到内容一致的脱敏结果和 manifest hash；archive 解包扫描无 fixture secret，raw 不存在。

## 2026-07-16 当前实现与证据

- 新增 skill 自带的零第三方依赖 validator：检查 frontmatter allowed keys、name/description、目录名、`agents/openai.yaml` 三个 interface 字段、`$skill-name` 引用和必需 references/scripts。
- `python3 .../scripts/quick_validate.py .codex/skills/kiro-prod-evidence-audit` 已通过。
- 官方 skill-creator `quick_validate.py` 仍在 import 阶段因宿主缺少 PyYAML 失败；这属于官方脚本环境依赖，未伪装成已通过。
- 打包器已修复 manifest 自引用、旧 archive 嵌套、symlink 越界和通用 URL credential 脱敏，并在 `SOURCE_DATE_EPOCH` 下规范化 gzip/tar 元数据。
- JSON/JSONL 先按规范化敏感 key 清理，再执行文本模式；第一次 archive 扫描曾发现带引号 JSON Authorization 未脱敏，修复后从头重跑。

固定 epoch 的最终 3 轮结果完全一致：

```text
manifest sha256 ba2d451d441d52b392cd917ddc06f2f7ff59e6f79913244852eb49e6897167a6
archive  sha256 971d9ce93d7aaa242a2d93f6d9eb2e32477ac9b7bfd834dd573a55f573af7b23
```

最终 archive 内容扫描：6 个成员；无 `raw/`；假 API key、Bearer、JWT、email、AWS key、PostgreSQL/Redis/HTTPS credentials、request body、bytes 和 refresh token 均不存在；对应 redaction marker 全部存在。

仍待发布总门禁复跑：从清理后的新 fixture 重新执行一次，并把命令、revision 和 dirty diff manifest 归档到 `feature/evidence/`。

## 残余风险与回滚

未知 secret 形态和未来 evidence source 仍可能不在 matcher 内；因此生产审计必须先用假 fixture 扩展规则，archive 默认继续排除 raw。回滚不得恢复 manifest 自引用、symlink 越界或 raw 默认入包；若确定性 metadata 影响兼容，可只回滚固定 epoch 输出，不回滚脱敏保护。
