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

// External pools speak the Claude Code / Anthropic protocol, so both the options shown and the
// model sent in a test request use standard Claude Code model ids (claude-opus-4-6), never
// Kiro's dotted ids (claude-opus-4.6), shorthand aliases (sonnet) or -thinking/[1m] variants.
export const EXTERNAL_POOL_TEST_MODELS: TestModelOption[] = [
  { id: 'claude-sonnet-5', label: 'claude-sonnet-5' },
  { id: 'claude-sonnet-4-6', label: 'claude-sonnet-4-6' },
  { id: 'claude-sonnet-4-5', label: 'claude-sonnet-4-5' },
  { id: 'claude-haiku-4-5', label: 'claude-haiku-4-5' },
  { id: 'claude-opus-5-5', label: 'claude-opus-5-5' },
  { id: 'claude-opus-5', label: 'claude-opus-5' },
  { id: 'claude-opus-4-8', label: 'claude-opus-4-8' },
  { id: 'claude-opus-4-7', label: 'claude-opus-4-7' },
  { id: 'claude-opus-4-6', label: 'claude-opus-4-6' },
  { id: 'claude-opus-4-5', label: 'claude-opus-4-5' },
  { id: 'claude-fable-5-1', label: 'claude-fable-5-1' },
]

export const DEFAULT_EXTERNAL_POOL_TEST_MODEL = 'claude-sonnet-4-5'

/**
 * Standard Claude Code model id for any spelling of a Claude model, or `null` when the value
 * is not a Claude model. `claude-opus-4.6`, `opus-4.6`, `claude-opus-4-6-thinking` and
 * `claude-opus-4-6[1m]` all become `claude-opus-4-6`; dated ids keep their date.
 */
export function toExternalPoolTestModelName(model: string): string | null {
  let value = model.trim().toLowerCase()
  if (!value) return null
  if (value.endsWith('[1m]')) value = value.slice(0, -4)
  if (value.endsWith('-thinking')) value = value.slice(0, -9)
  const legacy = value.match(/^claude-3[.-]5-(sonnet|haiku)(?:-(\d{8}))?$/)
  if (legacy) return `claude-3-5-${legacy[1]}${legacy[2] ? `-${legacy[2]}` : ''}`
  const match = value.match(/^(?:claude-)?(opus|sonnet|haiku|fable)-(\d+)(?:[.-](\d{1,2}))?(?:-(\d{8}))?$/)
  if (!match) return null
  const [, family, major, minor, date] = match
  return `claude-${family}-${major}${minor ? `-${minor}` : ''}${date ? `-${date}` : ''}`
}

export function buildExternalPoolTestModelOptions(
  catalogModels?: TestModelCatalogItem[],
  supportedModels?: string[]
): TestModelOption[] {
  const seen = new Set<string>()
  const options: TestModelOption[] = []
  const push = (id: string | null) => {
    if (!id || seen.has(id)) return
    seen.add(id)
    options.push({ id, label: id })
  }
  // An explicit allowlist restricts the pool; otherwise offer the standard set plus the
  // Claude models known to the catalog.
  const allowed = (supportedModels || []).map(toExternalPoolTestModelName)
  allowed.forEach(push)
  if (options.length > 0) return options
  EXTERNAL_POOL_TEST_MODELS.forEach((item) => push(item.id))
  ;[...(catalogModels || [])]
    .map((item) => toExternalPoolTestModelName(item.model))
    .sort()
    .forEach(push)
  return options
}

export function defaultExternalPoolTestModelForOptions(options: TestModelOption[]) {
  return (
    options.find((option) => option.id === DEFAULT_EXTERNAL_POOL_TEST_MODEL)?.id ||
    options[0]?.id ||
    DEFAULT_EXTERNAL_POOL_TEST_MODEL
  )
}
