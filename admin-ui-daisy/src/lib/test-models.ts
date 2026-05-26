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
