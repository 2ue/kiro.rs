export interface TestModelOption {
  id: string
  label: string
}

export const TEST_MODELS: TestModelOption[] = [
  { id: 'claude-opus-4-5-20251101', label: 'Claude Opus 4.5' },
  { id: 'claude-sonnet-4-5-20250929', label: 'Claude Sonnet 4.5' },
  { id: 'claude-haiku-4-5-20251001', label: 'Claude Haiku 4.5' },
  { id: 'claude-opus-4-6', label: 'Claude Opus 4.6' },
  { id: 'claude-sonnet-4-6', label: 'Claude Sonnet 4.6' },
  { id: 'claude-opus-4-7', label: 'Claude Opus 4.7' },
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
