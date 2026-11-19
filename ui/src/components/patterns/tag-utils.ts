// 标签输入的纯逻辑：与 main 的 credential-tags-field 同一套规则（去空白、去重、单个最长 64 字符、最多 32 个）。
// 上限只约束“新加入”的标签：账号上已有的标签（哪怕超过 32 个或超过 64 字符）原样保留，只做去空白去重，
// 避免打开表单再保存就把历史数据悄悄截掉。

export const MAX_TAGS = 32
export const MAX_TAG_LENGTH = 64

/** 新输入的单个标签：去空白并截断到 64 字符。已有标签不要走这里，用 cleanTags。 */
export function normalizeTag(value: string): string {
  return value.trim().slice(0, MAX_TAG_LENGTH)
}

/** 已有标签的清理：只去空白、去空项、去重（区分大小写），不截断长度、不限数量，顺序不变。 */
export function cleanTags(tags: readonly string[]): string[] {
  const seen = new Set<string>()
  const result: string[] = []
  for (const raw of tags) {
    const tag = raw.trim()
    if (!tag || seen.has(tag)) continue
    seen.add(tag)
    result.push(tag)
  }
  return result
}

/** 一组全新的标签（比如导入时文件里带的）：去空白、去重、截断到 64 字符，最多保留 32 个，顺序不变。 */
export function normalizeTags(tags: readonly string[]): string[] {
  return addTags([], tags.map(normalizeTag))
}

/** 把一段输入拆成标签：中英文逗号、分号和换行都算分隔，粘贴一串标签时一次加进去；每个都按新标签截断。 */
export function splitTagInput(text: string): string[] {
  return text.split(/[,，;；\n]+/).map(normalizeTag).filter(Boolean)
}

/**
 * 追加标签：已有标签原样保留（只去空白去重），新标签去空白、已有的不重复添加；
 * 数量上限只拦新标签——已有标签数达到 32 个后不再追加，但不会删掉已有的。
 * 新输入的长度截断在 splitTagInput 里做；从“已有标签”候选里点选的标签是池里现成的值，原样加入，
 * 这样同一个标签在不同账号上始终一致，按标签筛选和批量移除才能对上。
 */
export function addTags(current: readonly string[], incoming: readonly string[]): string[] {
  const result = cleanTags(current)
  const seen = new Set(result)
  for (const raw of incoming) {
    if (result.length >= MAX_TAGS) break
    const tag = raw.trim()
    if (!tag || seen.has(tag)) continue
    seen.add(tag)
    result.push(tag)
  }
  return result
}

/** 新标签里有几个因为数量上限没能加进去（用于提示）；已存在的、空白的不算。 */
export function tagsDroppedByLimit(current: readonly string[], incoming: readonly string[]): string[] {
  const next = new Set(addTags(current, incoming))
  return cleanTags(incoming).filter((tag) => !next.has(tag))
}

/** 移除标签（去空白后精确匹配，不截断，长标签也能移除），其余顺序不变。 */
export function removeTags(current: readonly string[], removing: readonly string[]): string[] {
  const drop = new Set(cleanTags(removing))
  return cleanTags(current).filter((tag) => !drop.has(tag))
}
