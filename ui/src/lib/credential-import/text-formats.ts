// KAM / kiro-manager-lite 的文本导出，转成和 JSON 账号同结构的对象，再交给 readAccount。
//
// - 卡密：`email----password----refreshToken----clientId----clientSecret----idp`，
//   分隔符也可以是 Tab 或两个以上空格；密码为 `no_password` 表示没有密码
// - CSV：首行是表头（可能带 BOM），按列名取值。KAM 写 `RefreshToken`，lite 写 `refreshToken`
// - TXT：`email,refreshToken[,nickname,idp]`，也可以用 `|` 分隔
// - 每行只有一个 refreshToken
// - 混在其中的 `ksk_` 行按 API Key 读

import type { JsonObject } from './values'

/** KAM 按 `-{4,}` 拆分，多出的横线留给前一段；这里取每串横线的最后 4 个作为分隔符。 */
const CARD_SEPARATOR = /-{4}(?!-)|\t| {2,}/

/** 表头列名（去掉空白、下划线、连字符后小写）→ 账号字段。 */
const CSV_COLUMNS: Record<string, string> = {
  邮箱: 'email',
  email: 'email',
  登录方式: 'provider',
  provider: 'provider',
  idp: 'provider',
  refreshtoken: 'refreshToken',
  accesstoken: 'accessToken',
  clientid: 'clientId',
  clientsecret: 'clientSecret',
  region: 'region',
  authmethod: 'authMethod',
  profilearn: 'profileArn',
}

export interface TextAccountRow {
  /** 第几行（从 1 开始），用于提示。 */
  line: number
  account: JsonObject
}

function splitCsvLine(line: string): string[] {
  const cells: string[] = []
  let cell = ''
  let quoted = false
  for (let index = 0; index < line.length; index++) {
    const char = line[index]
    if (quoted) {
      if (char === '"' && line[index + 1] === '"') {
        cell += '"'
        index++
      } else if (char === '"') {
        quoted = false
      } else {
        cell += char
      }
    } else if (char === '"') {
      quoted = true
    } else if (char === ',') {
      cells.push(cell)
      cell = ''
    } else {
      cell += char
    }
  }
  cells.push(cell)
  return cells.map((value) => value.trim())
}

function csvColumns(headerLine: string): Array<string | undefined> | null {
  const columns = splitCsvLine(headerLine).map((name) => CSV_COLUMNS[name.toLowerCase().replace(/[\s_-]/g, '')])
  const known = columns.filter(Boolean)
  return known.length >= 2 && known.includes('refreshToken') ? columns : null
}

function cardRow(segments: string[]): JsonObject {
  const [email, , refreshToken, clientId, clientSecret, idp] = segments
  const hasClient = Boolean(clientId && clientSecret)
  // 第 6 段缺失时，KAM 按有没有 clientId / clientSecret 区分 BuilderId 和 Google
  return { email, refreshToken, clientId, clientSecret, provider: idp || (hasClient ? 'BuilderId' : 'Google') }
}

function delimitedRow(segments: string[]): JsonObject {
  const [email, refreshToken, , idp] = segments
  return { email, refreshToken, provider: idp }
}

/** 文本里的每个账号行；不是任何已知文本格式时返回 null。 */
export function parseAccountTextRows(text: string): TextAccountRow[] | null {
  const lines = text.replace(/^\uFEFF/, '').split(/\r?\n/)
  const rows: TextAccountRow[] = []
  const firstIndex = lines.findIndex((line) => line.trim())
  if (firstIndex < 0) return null

  const columns = csvColumns(lines[firstIndex])
  if (columns) {
    lines.slice(firstIndex + 1).forEach((line, offset) => {
      if (!line.trim()) return
      const account: JsonObject = {}
      splitCsvLine(line).forEach((value, index) => {
        const key = columns[index]
        if (key && value) account[key] = value
      })
      rows.push({ line: firstIndex + offset + 2, account })
    })
    return rows
  }

  for (const [index, rawLine] of lines.entries()) {
    const line = rawLine.trim()
    if (!line || line.startsWith('#')) continue
    if (line.startsWith('ksk_')) {
      rows.push({ line: index + 1, account: { kiroApiKey: line } })
      continue
    }
    const card = line.split(CARD_SEPARATOR).map((value) => value.trim())
    if (card.length >= 3) {
      rows.push({ line: index + 1, account: cardRow(card) })
      continue
    }
    const delimited = line.split(/[,|]/).map((value) => value.trim())
    if (delimited.length >= 2 && delimited.length <= 4) {
      rows.push({ line: index + 1, account: delimitedRow(delimited) })
      continue
    }
    if (delimited.length === 1 && !/\s/.test(line)) {
      rows.push({ line: index + 1, account: { refreshToken: line } })
      continue
    }
    return null
  }
  return rows.length ? rows : null
}
