export interface TestModelOption {
  id: string
  label: string
}

// Local credential tests send Kiro upstream model names.
export const TEST_MODELS: TestModelOption[] = [
  { id: 'claude-sonnet-4.5', label: 'Claude Sonnet 4.5 (Kiro)' },
  { id: 'claude-haiku-4.5', label: 'Claude Haiku 4.5 (Kiro)' },
  { id: 'claude-opus-4.5', label: 'Claude Opus 4.5 (Kiro)' },
  { id: 'claude-sonnet-4.6', label: 'Claude Sonnet 4.6 (Kiro)' },
  { id: 'claude-opus-4.6', label: 'Claude Opus 4.6 (Kiro)' },
  { id: 'claude-opus-4.7', label: 'Claude Opus 4.7 (Kiro)' },
]

export const DEFAULT_TEST_MODEL = TEST_MODELS[0].id
export const DEFAULT_TEST_PROMPT = 'hi'

export function testModelLabel(model: string) {
  return TEST_MODELS.find((option) => option.id === model)?.label || model
}

function isAutoModel(model: string) {
  return model.trim().toLowerCase() === 'auto'
}

export interface TestModelCatalogItem {
  model: string
  displayName?: string
}

export function buildTestModelOptions(
  catalogModels?: TestModelCatalogItem[],
  supportedModels?: string[]
): TestModelOption[] {
  const seen = new Set<string>()
  const options: TestModelOption[] = []
  const push = (id: string, label?: string) => {
    const model = id.trim()
    if (!model) return
    const key = model.toLowerCase()
    if (seen.has(key)) return
    seen.add(key)
    options.push({ id: model, label: label?.trim() || testModelLabel(model) })
  }

  ;(supportedModels || []).forEach((model) => push(model))
  ;[...(catalogModels || [])]
    .sort((left, right) => left.model.localeCompare(right.model))
    .forEach((item) => push(item.model, item.displayName || testModelLabel(item.model)))
  TEST_MODELS.forEach((item) => push(item.id, item.label))

  return options.sort((left, right) => Number(isAutoModel(left.id)) - Number(isAutoModel(right.id)))
}

export function defaultTestModelForOptions(options: TestModelOption[]) {
  return options.find((option) => !isAutoModel(option.id))?.id || DEFAULT_TEST_MODEL
}

// External pools are tested through the Claude Code/Anthropic request surface.
// Their allowlists and test requests therefore use Claude Code command names.
export const CLAUDE_CODE_TEST_MODELS: TestModelOption[] = [
  { id: 'sonnet', label: 'Sonnet (Claude Code)' },
  { id: 'sonnet-4.5', label: 'Sonnet 4.5 (Claude Code)' },
  { id: 'sonnet-4.6', label: 'Sonnet 4.6 (Claude Code)' },
  { id: 'haiku', label: 'Haiku (Claude Code)' },
  { id: 'haiku-4.5', label: 'Haiku 4.5 (Claude Code)' },
  { id: 'opus', label: 'Opus (Claude Code)' },
  { id: 'opus-4.5', label: 'Opus 4.5 (Claude Code)' },
  { id: 'opus-4.6', label: 'Opus 4.6 (Claude Code)' },
  { id: 'opus-4.7', label: 'Opus 4.7 (Claude Code)' },
]

export const DEFAULT_CLAUDE_CODE_TEST_MODEL = CLAUDE_CODE_TEST_MODELS[0].id

export function claudeCodeModelLabel(model: string) {
  return CLAUDE_CODE_TEST_MODELS.find((option) => option.id === model)?.label || model
}

export function toClaudeCodeModelName(model: string) {
  const original = model.trim()
  if (!original) return ''
  const normalized = original.toLowerCase()
  const oneM = normalized.endsWith('[1m]')
  const withoutOneM = oneM ? normalized.slice(0, -4) : normalized
  const thinking = withoutOneM.endsWith('-thinking')
  const base = thinking ? withoutOneM.slice(0, -9) : withoutOneM
  const legacyMatch = base.match(/^claude-(?:3-5|3\.5)-(sonnet|haiku)(?:-\d{8})?$/)
  if (legacyMatch) {
    const [, family] = legacyMatch
    const suffix = `${thinking ? '-thinking' : ''}${oneM ? '[1m]' : ''}`
    return `${family}-3.5${suffix}`
  }
  const match = base.match(/^claude-(opus|sonnet|haiku)-(\d+)(?:[.-](\d+))?(?:-(\d{8}))?$/)
  if (!match) return original
  const [, family, major, minor] = match
  const version = minor ? `${major}.${minor}` : major
  const suffix = `${thinking ? '-thinking' : ''}${oneM ? '[1m]' : ''}`
  return `${family}-${version}${suffix}`
}

export function buildClaudeCodeTestModelOptions(
  catalogModels?: TestModelCatalogItem[],
  supportedModels?: string[]
): TestModelOption[] {
  const seen = new Set<string>()
  const options: TestModelOption[] = []
  const push = (id: string, label?: string) => {
    const model = toClaudeCodeModelName(id)
    if (!model) return
    const key = model.toLowerCase()
    if (seen.has(key)) return
    seen.add(key)
    options.push({
      id: model,
      label: label?.trim() || claudeCodeModelLabel(model),
    })
  }

  ;(supportedModels || []).forEach((model) => push(model))
  ;[...(catalogModels || [])]
    .sort((left, right) => left.model.localeCompare(right.model))
    .forEach((item) => push(item.model, item.displayName || claudeCodeModelLabel(toClaudeCodeModelName(item.model))))
  CLAUDE_CODE_TEST_MODELS.forEach((item) => push(item.id, item.label))

  return options.sort((left, right) => Number(isAutoModel(left.id)) - Number(isAutoModel(right.id)))
}

export function defaultClaudeCodeTestModelForOptions(options: TestModelOption[]) {
  return options.find((option) => !isAutoModel(option.id))?.id || DEFAULT_CLAUDE_CODE_TEST_MODEL
}
