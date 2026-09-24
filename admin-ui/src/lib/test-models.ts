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

// External pool tests use Claude Code protocol model IDs: keep the `claude-`
// prefix and use hyphen-separated versions such as `claude-opus-4-8`.
export const EXTERNAL_POOL_TEST_MODELS: TestModelOption[] = [
  { id: 'claude-sonnet-5', label: 'Claude Sonnet 5' },
  { id: 'claude-sonnet-4-6', label: 'Claude Sonnet 4.6' },
  { id: 'claude-sonnet-4-5', label: 'Claude Sonnet 4.5' },
  { id: 'claude-haiku-4-5', label: 'Claude Haiku 4.5' },
  { id: 'claude-opus-5', label: 'Claude Opus 5' },
  { id: 'claude-opus-4-8', label: 'Claude Opus 4.8' },
  { id: 'claude-opus-4-7', label: 'Claude Opus 4.7' },
  { id: 'claude-opus-4-6', label: 'Claude Opus 4.6' },
  { id: 'claude-opus-4-5', label: 'Claude Opus 4.5' },
]

export const DEFAULT_EXTERNAL_POOL_TEST_MODEL = EXTERNAL_POOL_TEST_MODELS[0].id

export function externalPoolModelLabel(model: string) {
  return EXTERNAL_POOL_TEST_MODELS.find((option) => option.id === model)?.label || model
}

export function toExternalPoolTestModelName(model: string) {
  const original = model.trim()
  if (!original) return ''
  if (isAutoModel(original)) return original

  const oneM = original.endsWith('[1m]')
  const withoutOneM = oneM ? original.slice(0, -4) : original
  const thinking = withoutOneM.endsWith('-thinking')
  const base = thinking ? withoutOneM.slice(0, -9) : withoutOneM
  const match = base.match(/^(?:claude-)?(opus|sonnet|haiku)-(\d+)(?:[.-](\d+))?(?:-(\d{6,}))?$/i)
  if (!match) return original

  const [, family, major, minor, date] = match
  const suffix = `${date ? `-${date}` : ''}${thinking ? '-thinking' : ''}${oneM ? '[1m]' : ''}`
  return `claude-${family.toLowerCase()}-${major}${minor ? `-${minor}` : ''}${suffix}`
}

export function buildExternalPoolTestModelOptions(
  catalogModels?: TestModelCatalogItem[],
  supportedModels?: string[]
): TestModelOption[] {
  const seen = new Set<string>()
  const options: TestModelOption[] = []
  const push = (id: string, label?: string) => {
    const model = toExternalPoolTestModelName(id)
    if (!model) return
    const key = model.toLowerCase()
    if (seen.has(key)) return
    seen.add(key)
    options.push({
      id: model,
      label: label?.trim() || externalPoolModelLabel(model),
    })
  }

  ;(supportedModels || []).forEach((model) => push(model))
  ;[...(catalogModels || [])]
    .sort((left, right) => left.model.localeCompare(right.model))
    .forEach((item) => push(item.model, item.displayName || externalPoolModelLabel(item.model)))
  EXTERNAL_POOL_TEST_MODELS.forEach((item) => push(item.id, item.label))

  return options.sort((left, right) => Number(isAutoModel(left.id)) - Number(isAutoModel(right.id)))
}

export function defaultExternalPoolTestModelForOptions(options: TestModelOption[]) {
  return options.find((option) => !isAutoModel(option.id))?.id || DEFAULT_EXTERNAL_POOL_TEST_MODEL
}
