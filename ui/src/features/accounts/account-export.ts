// 从配置迁移包（GET /config/export）里挑出要导出的账号，整理成能被批量导入识别的 JSON 数组。
import { ACCOUNT_REQUEST_FIELDS, REDACTED } from './account-import.ts'

export type ExportScope = 'selected' | 'filtered' | 'all'

/**
 * 按 ID 挑账号（ids 为 null 表示全部），只保留创建账号接口的字段，顺序按 ids 给出的顺序；
 * 包里没有的 ID（比如刚被删除）直接跳过。
 */
export function pickExportAccounts(packageAccounts: readonly unknown[], ids: readonly number[] | null): Record<string, unknown>[] {
  const byId = new Map<number, Record<string, unknown>>()
  const ordered: Record<string, unknown>[] = []
  for (const item of packageAccounts) {
    if (!item || typeof item !== 'object') continue
    const record = item as Record<string, unknown>
    const picked: Record<string, unknown> = {}
    for (const field of ACCOUNT_REQUEST_FIELDS) {
      if (record[field] !== undefined && record[field] !== null) picked[field] = record[field]
    }
    const id = Number(record.id)
    if (Number.isInteger(id)) byId.set(id, picked)
    ordered.push(picked)
  }
  if (ids === null) return ordered
  return ids.flatMap((id) => {
    const picked = byId.get(id)
    return picked ? [picked] : []
  })
}

/** 导出的账号里是否还有脱敏的密钥（不带密钥导出时为 true，这样的文件不能直接导入）。 */
export function exportHasRedactedSecrets(accounts: readonly Record<string, unknown>[]): boolean {
  return accounts.some((account) => account.apiKey === REDACTED)
}

export function accountExportFilename(now: Date = new Date()): string {
  const pad = (value: number) => String(value).padStart(2, '0')
  const stamp = `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}-${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}`
  return `claude-pool-accounts-${stamp}.json`
}
