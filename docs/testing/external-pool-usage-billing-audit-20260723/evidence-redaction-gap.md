# 生产证据包脱敏缺口独立记录 - 2026-07-23

## 问题

本轮生产 usage 审计生成的初版长期报告和部分 `redacted/` 证据文件仍包含生产实例标识，例如生产 IP、公开域名、部署目录、容器名或公开端口。

这不是 kiro.rs runtime 的 usage 计费问题，而是生产取证流程里的脱敏覆盖缺口，必须独立记录。

## 影响

如果把初版报告或初版 redacted 证据包直接外发，可能暴露生产入口和部署拓扑信息。

本轮检查没有发现 SSH 密码、API key、Authorization header 或完整请求体被写入长期报告。用户提供的 SSH 密码没有写入文档。`raw/` 目录本来就只作为本地原始材料保存，默认归档不应包含 `raw/`。

## 发现位置

发现方式：

```text
rg -n "<prod-ip-pattern>|<prod-domain-pattern>|<compose-project-pattern>|<password-pattern>|Authorization|Bearer|api[-_ ]?key" \
  docs/testing/external-pool-usage-billing-audit-20260723/audit.en.md \
  tmp/prod-evidence/20260723-003327-kiro-prod-usage-zero \
  -g '!raw/**' -g '!*.tar.gz'
```

命中类型：

```text
docs/testing/external-pool-usage-billing-audit-20260723/audit.en.md:
  production deployment directory
  production container names
  production public domain

tmp/prod-evidence/.../summary/inventory.md:
  production deployment directory
  app container name
  public edge hostname

tmp/prod-evidence/.../redacted/:
  health probe IP
  Docker Compose project/container names
  Caddy public hostname
  published port and upstream container target

tmp/prod-evidence/.../problems/P001-.../problem.md:
  production public endpoint
```

## 已执行的本地修正

长期英文报告已把生产入口、部署目录、容器名改成脱敏占位：

```text
<prod-deployment-redacted>
<prod-app-container-redacted>
<prod-postgres-container-redacted>
<prod-redis-container-redacted>
<prod-public-entry-redacted>
```

中文主报告从一开始只使用脱敏生产上下文，不写生产 IP、生产域名、SSH 密码或 API key。

## 还需要补的流程修复

`.codex/skills/kiro-prod-evidence-audit/scripts/package_evidence.py` 当前主要覆盖 token/password/API key/数据库 URL/JWT 等 secret 模式，但没有通用覆盖：

```text
IPv4 / IPv6 地址
生产公开域名
Docker Compose project name
生产容器名
部署目录
published host port
```

后续应该把这些作为 evidence redaction 的可配置项，而不是只靠人工 `rg`。

建议方案：

```text
1. evidence run 初始化时写入 local-only redaction-overrides.json。
2. overrides 中保存本次生产 host、public domain、deployment dir、compose project、container names、published ports。
3. package_evidence.py 读取 overrides，只在 redacted/ 和默认 archive 中替换。
4. raw/ 保持本地原始材料，不进入默认 archive。
5. package 后自动运行 leak scan，命中高风险模式时失败并拒绝生成可分享 archive。
```

建议 leak scan：

```text
rg -n "<prod-ip>|<prod-domain>|<compose-project>|Authorization|Bearer|api_key|password|postgres://" \
  <evidence-root> \
  -g '!raw/**' \
  -g '!*.tar.gz'
```

## 分享限制

在完成上述脱敏流程修复和重新打包前，不应把初版 redacted archive 当成可公开分享材料。

本地可以继续保留 raw 证据供工程分析使用，但所有对外报告只引用脱敏摘要、聚合数值、源码路径和 request id 级别的非 secret 证据。
