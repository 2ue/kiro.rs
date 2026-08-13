#!/usr/bin/env node

import fs from 'node:fs'
import path from 'node:path'
import process from 'node:process'

const repoRoot = path.resolve(import.meta.dirname, '../..')
const issuesRoot = path.join(repoRoot, 'feature/issues')

const contracts = [
  ['status', /^Status:\s*\S/im],
  ['severity', /^Severity:\s*\S/im],
  ['symptom_or_impact', /^##\s+.*(?:问题|现象|影响|范围与结论|结论|scope|impact|symptom)/imu],
  ['root_cause', /^##\s+.*(?:根因|原因|源码链|代码链|root cause)/imu],
  ['reproduction', /^##\s+.*(?:复现|重现|reproduction)/imu],
  ['solution', /^##\s+.*(?:方案|修复|优化|selected fix|tradeoff|alternative)/imu],
  ['validation_or_evidence', /^##\s+.*(?:验收|验证|测试|结果|证据|回归|acceptance|verified|evidence)/imu],
  ['risk_or_rollback', /^##\s+.*(?:残余|风险|限制|边界|回滚|residual|risk|rollback|performance bound)/imu],
]

const markdownFiles = fs
  .readdirSync(issuesRoot, { withFileTypes: true })
  .filter((entry) => entry.isFile() && entry.name.endsWith('.md') && entry.name !== 'README.md')
  .map((entry) => path.join(issuesRoot, entry.name))
  .sort()

const failures = []
let checkedLinks = 0

for (const file of markdownFiles) {
  const source = fs.readFileSync(file, 'utf8')
  const relativeFile = path.relative(repoRoot, file)

  for (const [field, pattern] of contracts) {
    if (!pattern.test(source)) {
      failures.push({ file: relativeFile, kind: 'missing_contract_field', field })
    }
  }

  for (const match of source.matchAll(/\[[^\]]+\]\(([^)]+)\)/g)) {
    const rawTarget = match[1].trim()
    if (
      rawTarget.startsWith('#') ||
      /^[a-z][a-z0-9+.-]*:/iu.test(rawTarget) ||
      rawTarget.includes('<redacted>')
    ) {
      continue
    }
    const targetWithoutFragment = rawTarget.split('#', 1)[0]
    if (!targetWithoutFragment) continue
    checkedLinks += 1
    const target = path.resolve(path.dirname(file), decodeURIComponent(targetWithoutFragment))
    if (!fs.existsSync(target)) {
      failures.push({
        file: relativeFile,
        kind: 'missing_relative_link',
        target: rawTarget,
      })
    }
  }
}

const report = {
  issuesRoot: path.relative(repoRoot, issuesRoot),
  filesChecked: markdownFiles.length,
  linksChecked: checkedLinks,
  failureCount: failures.length,
  failures,
}

if (process.argv.includes('--json')) {
  process.stdout.write(`${JSON.stringify(report, null, 2)}\n`)
} else if (failures.length === 0) {
  process.stdout.write(
    `PASS: ${report.filesChecked} issue documents satisfy the section contract; ${checkedLinks} relative links resolve.\n`,
  )
} else {
  process.stderr.write(
    `FAIL: ${failures.length} documentation contract violation(s) across ${report.filesChecked} issue documents.\n`,
  )
  for (const failure of failures) {
    const detail = failure.field ?? failure.target
    process.stderr.write(`- ${failure.file}: ${failure.kind}: ${detail}\n`)
  }
}

process.exitCode = failures.length === 0 ? 0 : 1
