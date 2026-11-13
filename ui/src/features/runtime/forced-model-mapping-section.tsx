import { useMemo, useState, type ReactNode } from 'react'
import { ChevronDown, Pencil, Plus, RotateCcw, Trash2, Zap } from 'lucide-react'
import { ModalShell, useConfirm } from '@/components/patterns'
import {
  Button,
  Checkbox,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
  Input,
  Label,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Switch,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  Textarea,
} from '@/components/ui'
import {
  canonicalForcedRoutePrefix,
  defaultForcedModelMapping,
  DEFAULT_FORCED_HAIKU_TARGET,
  DEFAULT_FORCED_SONNET_TARGET,
  FORCED_CONTEXT_BACKOFF_LIMITS,
  FORCED_MODEL_FAMILIES,
  forcedModelMappingAnyRouteEnabled,
  parseForcedKnowledgeTopics,
  shadowedForcedFamilies,
} from '@/lib/runtime-config-defaults'
import type {
  ForcedContextBackoffConfig,
  ForcedKnowledgeEntry,
  ForcedKnowledgeLocale,
  ForcedModelCatalog,
  ForcedModelCatalogItem,
  ForcedModelMappingConfig,
  ForcedModelRule,
  ModelCapabilitiesStatus,
} from '@/types/api'
import { useForcedModelCatalog, useForcedModelKnowledge, useForcedModelTargets } from '@/hooks/use-usage'
import { NumField, TogField } from './runtime-sections'

const FAMILY_LABEL: Record<string, string> = {
  opus: 'Opus',
  fable: 'Fable',
  mythos: 'Mythos',
  sonnet: 'Sonnet',
  haiku: 'Haiku',
}

type LocaleChoice = 'any' | ForcedKnowledgeLocale

const LOCALE_LABEL: Record<LocaleChoice, string> = {
  any: '不限',
  en: '英文',
  zh: '中文',
}

function familyLabel(family: string): string {
  return FAMILY_LABEL[family] ?? family
}

/** 点号和横杠两种版本写法视为同一个模型。只用于界面提示，是否可用以后端保存时的校验为准。 */
function looseModelKey(model: string): string {
  return model.trim().toLowerCase().replace(/\./g, '-')
}

function formatTokens(value?: number): string {
  if (!value || value <= 0) return ''
  if (value >= 1_000_000) return `${Number((value / 1_000_000).toFixed(1))}M`
  if (value >= 1_000) return `${Math.round(value / 1_000)}K`
  return String(value)
}

type CatalogOption = { model: string; maxInputTokens?: number; source?: string }

/** 官方 Claude、Kiro 同步和手动模型目录合并后供配置选择。 */
function catalogOptions(capabilities?: ModelCapabilitiesStatus, catalog?: ForcedModelCatalog): CatalogOption[] {
  const seen = new Set<string>()
  const options: CatalogOption[] = []
  const add = (item: ForcedModelCatalogItem) => {
    const model = item.model.trim()
    const key = model.toLowerCase()
    if (!model || seen.has(key)) return
    seen.add(key)
    options.push({ model, maxInputTokens: item.maxInputTokens, source: item.source })
  }
  for (const item of catalog?.official ?? []) add(item)
  for (const item of catalog?.kiro ?? []) add(item)
  for (const item of catalog?.custom ?? []) add(item)
  for (const item of capabilities?.models ?? []) add(item)
  return options.sort((a, b) => {
    const ac = a.model.toLowerCase().startsWith('claude') ? 0 : 1
    const bc = b.model.toLowerCase().startsWith('claude') ? 0 : 1
    return ac - bc || a.model.localeCompare(b.model)
  })
}

function catalogSourceLabel(source?: string): string | null {
  if (source === 'claude-official') return 'Claude 官方'
  if (source === 'manual') return '自定义'
  if (source) return 'Kiro 上游'
  return null
}

function canonicalModelKey(model: string): string {
  return looseModelKey(model).toLowerCase()
}

function hasCatalogModel(options: CatalogOption[], model: string): boolean {
  return options.some((option) => canonicalModelKey(option.model) === canonicalModelKey(model))
}

function routeStatusSummary(mapping: ForcedModelMappingConfig): string {
  const overrides = Object.entries(mapping.pathOverrides)
  const on = overrides.filter(([, route]) => route.enabled === true).map(([prefix]) => prefix)
  const off = overrides.filter(([, route]) => route.enabled === false).map(([prefix]) => prefix)
  if (mapping.enabled) return off.length > 0 ? `所有路由开启，${off.join('、')} 除外` : '所有路由开启'
  return on.length > 0 ? `仅 ${on.join('、')} 开启` : '未开启'
}

function SubBlock({ title, desc, actions, children }: { title: string; desc?: string; actions?: ReactNode; children: ReactNode }) {
  return (
    <div className="space-y-3 border-t border-border/70 pt-5">
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div className="min-w-0">
          <div className="text-sm font-semibold">{title}</div>
          {desc && <div className="mt-1 max-w-3xl text-xs leading-5 text-muted-foreground">{desc}</div>}
        </div>
        {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
      </div>
      {children}
    </div>
  )
}

function DeleteButton({ title, size = 'icon-sm', onClick }: { title: string; size?: 'icon-sm' | 'icon-xs'; onClick: () => void }) {
  return (
    <Button
      variant="ghost"
      size={size}
      className="text-destructive hover:text-destructive"
      title={title}
      aria-label={title}
      onClick={onClick}
    >
      <Trash2 />
    </Button>
  )
}

function OnOffSelect({
  value,
  ariaLabel,
  className,
  onChange,
}: {
  value: boolean
  ariaLabel: string
  className?: string
  onChange: (next: boolean) => void
}) {
  return (
    <Select value={value ? 'on' : 'off'} onValueChange={(next) => onChange(next === 'on')}>
      <SelectTrigger size="sm" className={className} aria-label={ariaLabel}>
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value="on">开启</SelectItem>
        <SelectItem value="off">关闭</SelectItem>
      </SelectContent>
    </Select>
  )
}

// ─── 目标模型：可直接输入，也可从上游模型目录选择 ───────────────────────────────

function TargetModelInput({
  value,
  options,
  ariaLabel,
  onChange,
}: {
  value: string
  options: CatalogOption[]
  ariaLabel: string
  onChange: (next: string) => void
}) {
  const trimmed = value.trim()
  const inCatalog = !trimmed || options.length === 0 || hasCatalogModel(options, trimmed)
  return (
    <div className="min-w-0 space-y-1">
      <div className="flex min-w-0 items-center gap-1.5">
        <Input
          className="h-8 min-w-0 font-mono text-xs"
          value={value}
          placeholder="claude-sonnet-4-5"
          aria-label={ariaLabel}
          onChange={(event) => onChange(event.target.value)}
        />
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant="outline"
              size="sm"
              className="shrink-0"
              disabled={options.length === 0}
              title={options.length === 0 ? '模型目录为空，请直接输入模型名' : '从上游模型目录选择'}
            >
              选择
              <ChevronDown />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-80">
            <DropdownMenuLabel>上游模型目录</DropdownMenuLabel>
            {options.map((option) => (
              <DropdownMenuItem key={option.model} onSelect={() => onChange(option.model)}>
                <span className="min-w-0 flex-1 truncate font-mono text-xs">{option.model}</span>
                {catalogSourceLabel(option.source) && (
                  <span className="shrink-0 text-[11px] text-muted-foreground">{catalogSourceLabel(option.source)}</span>
                )}
                {option.maxInputTokens ? (
                  <span className="shrink-0 text-[11px] text-muted-foreground">输入 {formatTokens(option.maxInputTokens)}</span>
                ) : null}
              </DropdownMenuItem>
            ))}
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
      {!trimmed && <div className="text-[11px] leading-4 text-destructive">目标模型不能为空</div>}
      {!inCatalog && <div className="text-[11px] leading-4 text-warning">模型目录里没有同名模型，保存时后端会校验是否可用。</div>}
    </div>
  )
}

// ─── 收敛规则 ────────────────────────────────────────────────────────────────

function FamilyChecklist({ value, onChange }: { value: string[]; onChange: (next: string[]) => void }) {
  // 后端返回了界面不认识的族时也列出来，方便取消勾选。
  const known = FORCED_MODEL_FAMILIES as string[]
  const options = [...known, ...value.filter((family) => !known.includes(family))]
  const selected = new Set(value)
  const toggle = (family: string, checked: boolean) => {
    const next = new Set(selected)
    if (checked) next.add(family)
    else next.delete(family)
    onChange(options.filter((item) => next.has(item)))
  }
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
      {options.map((family) => (
        <label key={family} className="flex cursor-pointer items-center gap-1.5 text-xs text-foreground">
          <Checkbox
            checked={selected.has(family)}
            onCheckedChange={(checked) => toggle(family, checked === true)}
            aria-label={familyLabel(family)}
          />
          {familyLabel(family)}
        </label>
      ))}
    </div>
  )
}

function RulesTable({
  rules,
  options,
  onChange,
}: {
  rules: ForcedModelRule[]
  options: CatalogOption[]
  onChange: (next: ForcedModelRule[]) => void
}) {
  const shadowed = useMemo(() => shadowedForcedFamilies(rules), [rules])
  const uncovered = useMemo(() => {
    const covered = new Set(rules.flatMap((rule) => rule.families.map((family) => family.trim().toLowerCase())))
    return FORCED_MODEL_FAMILIES.filter((family) => !covered.has(family))
  }, [rules])
  const update = (index: number, patch: Partial<ForcedModelRule>) =>
    onChange(rules.map((rule, ruleIndex) => (ruleIndex === index ? { ...rule, ...patch } : rule)))
  const remove = (index: number) => onChange(rules.filter((_, ruleIndex) => ruleIndex !== index))

  return (
    <div className="space-y-2">
      <div className="overflow-hidden rounded-lg border bg-card">
        <Table className="min-w-[640px]">
          <TableHeader>
            <TableRow>
              <TableHead className="w-14">顺序</TableHead>
              <TableHead>模型族</TableHead>
              <TableHead className="w-[22rem]">目标模型</TableHead>
              <TableHead className="w-12" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {rules.length === 0 ? (
              <TableRow>
                <TableCell colSpan={4} className="py-6 text-center text-sm text-muted-foreground">
                  没有规则，所有 Claude 模型都改发到默认目标模型。
                </TableCell>
              </TableRow>
            ) : (
              rules.map((rule, index) => (
                <TableRow key={index}>
                  <TableCell className="align-top text-xs tabular-nums text-muted-foreground">#{index + 1}</TableCell>
                  <TableCell className="whitespace-normal align-top">
                    <div className="flex min-h-8 items-center">
                      <FamilyChecklist value={rule.families} onChange={(families) => update(index, { families })} />
                    </div>
                    {rule.families.length === 0 && <div className="mt-1 text-[11px] text-destructive">至少选择一个模型族</div>}
                  </TableCell>
                  <TableCell className="whitespace-normal align-top">
                    <TargetModelInput
                      value={rule.target}
                      options={options}
                      ariaLabel={`第 ${index + 1} 条规则的目标模型`}
                      onChange={(target) => update(index, { target })}
                    />
                  </TableCell>
                  <TableCell className="align-top">
                    <DeleteButton title="删除规则" onClick={() => remove(index)} />
                  </TableCell>
                </TableRow>
              ))
            )}
          </TableBody>
        </Table>
      </div>
      {shadowed.length > 0 && (
        <div className="text-xs leading-5 text-warning">
          {shadowed
            .map(
              (item) =>
                `${familyLabel(item.family)} 出现在第 ${[item.winner, ...item.shadowed].join('、')} 条规则里，只有第 ${item.winner} 条生效`,
            )
            .join('；')}
          。
        </div>
      )}
      {uncovered.length > 0 && (
        <div className="text-xs leading-5 text-muted-foreground">
          {uncovered.map(familyLabel).join('、')} 没有对应规则，改发到默认目标模型。
        </div>
      )}
    </div>
  )
}

function ModelOverridesTable({
  overrides,
  options,
  onChange,
}: {
  overrides: Record<string, string>
  options: CatalogOption[]
  onChange: (next: Record<string, string>) => void
}) {
  const [model, setModel] = useState('')
  const [target, setTarget] = useState('')
  const entries = Object.entries(overrides)

  const add = () => {
    const normalizedModel = canonicalModelKey(model)
    const normalizedTarget = target.trim().toLowerCase()
    if (!normalizedModel || !normalizedTarget) return
    onChange({ ...overrides, [normalizedModel]: normalizedTarget })
    setModel('')
    setTarget('')
  }

  const updateModel = (previous: string, next: string) => {
    const normalized = canonicalModelKey(next)
    const previousTarget = overrides[previous]
    const nextOverrides = { ...overrides }
    delete nextOverrides[previous]
    if (normalized) nextOverrides[normalized] = previousTarget ?? ''
    onChange(nextOverrides)
  }

  const updateTarget = (modelKey: string, next: string) =>
    onChange({ ...overrides, [modelKey]: next })

  return (
    <div className="space-y-2">
      <div className="overflow-hidden rounded-lg border bg-card">
        <Table className="min-w-[760px]">
          <TableHeader>
            <TableRow>
              <TableHead>客户端模型</TableHead>
              <TableHead>映射目标</TableHead>
              <TableHead className="w-12" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {entries.length === 0 ? (
              <TableRow>
                <TableCell colSpan={3} className="py-6 text-center text-sm text-muted-foreground">
                  没有单模型覆盖，所有模型按模型族规则或默认目标处理。
                </TableCell>
              </TableRow>
            ) : (
              entries.map(([modelKey, targetModel]) => (
                <TableRow key={modelKey}>
                  <TableCell className="align-top">
                    <TargetModelInput
                      value={modelKey}
                      options={options}
                      ariaLabel={`${modelKey} 的客户端模型`}
                      onChange={(next) => updateModel(modelKey, next)}
                    />
                  </TableCell>
                  <TableCell className="align-top">
                    <TargetModelInput
                      value={targetModel}
                      options={options}
                      ariaLabel={`${modelKey} 的映射目标`}
                      onChange={(next) => updateTarget(modelKey, next)}
                    />
                  </TableCell>
                  <TableCell className="align-top">
                    <DeleteButton
                      title="删除单模型覆盖"
                      onClick={() => {
                        const next = { ...overrides }
                        delete next[modelKey]
                        onChange(next)
                      }}
                    />
                  </TableCell>
                </TableRow>
              ))
            )}
          </TableBody>
        </Table>
      </div>
      <div className="flex flex-wrap items-end gap-2">
        <div className="min-w-[14rem] flex-1">
          <div className="mb-1 text-[11px] text-muted-foreground">客户端模型</div>
          <TargetModelInput value={model} options={options} ariaLabel="新增单模型覆盖的客户端模型" onChange={setModel} />
        </div>
        <div className="min-w-[14rem] flex-1">
          <div className="mb-1 text-[11px] text-muted-foreground">映射目标</div>
          <TargetModelInput value={target} options={options} ariaLabel="新增单模型覆盖的映射目标" onChange={setTarget} />
        </div>
        <Button size="sm" variant="outline" disabled={!model.trim() || !target.trim()} onClick={add}>
          <Plus />
          添加覆盖
        </Button>
      </div>
    </div>
  )
}

// ─── 路由覆盖 ────────────────────────────────────────────────────────────────

function RouteOverridesTable({
  mapping,
  routes,
  onChange,
}: {
  mapping: ForcedModelMappingConfig
  routes: string[]
  onChange: (next: ForcedModelMappingConfig) => void
}) {
  const [prefix, setPrefix] = useState('')
  const [enabled, setEnabled] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const entries = Object.entries(mapping.pathOverrides)
  const candidates = routes.filter((route) => !(route in mapping.pathOverrides))

  const setRoute = (route: string, value: boolean) =>
    onChange({ ...mapping, pathOverrides: { ...mapping.pathOverrides, [route]: { enabled: value } } })
  const removeRoute = (route: string) => {
    const pathOverrides = { ...mapping.pathOverrides }
    delete pathOverrides[route]
    onChange({ ...mapping, pathOverrides })
  }
  const add = () => {
    if (!prefix.trim()) return setError('请填写路由前缀')
    const route = canonicalForcedRoutePrefix(prefix)
    if (route in mapping.pathOverrides) return setError(`${route} 已有覆盖，直接在上面的列表里修改`)
    setRoute(route, enabled)
    setPrefix('')
    setError(null)
  }

  return (
    <div className="space-y-2">
      <div className="overflow-hidden rounded-lg border bg-card">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>路由前缀</TableHead>
              <TableHead className="w-40">强制收敛</TableHead>
              <TableHead className="w-12" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {entries.length === 0 ? (
              <TableRow>
                <TableCell colSpan={3} className="py-5 text-center text-sm text-muted-foreground">
                  没有路由覆盖，所有路由都跟随总开关。
                </TableCell>
              </TableRow>
            ) : (
              entries.map(([route, value]) => (
                <TableRow key={route}>
                  <TableCell className="font-mono text-xs">{route}</TableCell>
                  <TableCell>
                    <OnOffSelect
                      value={value.enabled === true}
                      ariaLabel={`${route} 的强制收敛`}
                      onChange={(next) => setRoute(route, next)}
                    />
                  </TableCell>
                  <TableCell>
                    <DeleteButton title="删除覆盖" onClick={() => removeRoute(route)} />
                  </TableCell>
                </TableRow>
              ))
            )}
          </TableBody>
        </Table>
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <div className="flex min-w-[14rem] flex-1 items-center gap-1.5 sm:max-w-sm">
          <Input
            className="h-8 font-mono text-xs"
            value={prefix}
            placeholder="/cc"
            aria-label="新增覆盖的路由前缀"
            onChange={(event) => {
              setPrefix(event.target.value)
              setError(null)
            }}
            onKeyDown={(event) => {
              if (event.key !== 'Enter') return
              event.preventDefault()
              add()
            }}
          />
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="outline" size="sm" className="shrink-0" disabled={candidates.length === 0} title="从已知路由选择">
                已知路由
                <ChevronDown />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              {candidates.map((route) => (
                <DropdownMenuItem
                  key={route}
                  className="font-mono text-xs"
                  onSelect={() => {
                    setPrefix(route)
                    setError(null)
                  }}
                >
                  {route}
                </DropdownMenuItem>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
        <OnOffSelect value={enabled} ariaLabel="新增覆盖的开关" className="w-24" onChange={setEnabled} />
        <Button size="sm" variant="outline" onClick={add}>
          <Plus />
          添加覆盖
        </Button>
      </div>
      {error && <div className="text-xs text-destructive">{error}</div>}
    </div>
  )
}

// ─── 知识条目 ────────────────────────────────────────────────────────────────

type KnowledgeDraft = {
  /** 编辑的条目序号；新增时为 null。 */
  index: number | null
  id: string
  enabled: boolean
  topicsText: string
  locale: LocaleChoice
  fact: string
}

function emptyKnowledgeDraft(): KnowledgeDraft {
  return { index: null, id: '', enabled: true, topicsText: '', locale: 'any', fact: '' }
}

function draftFromEntry(entry: ForcedKnowledgeEntry, index: number): KnowledgeDraft {
  return {
    index,
    id: entry.id,
    enabled: entry.enabled,
    topicsText: entry.topics.join(', '),
    locale: entry.locale ?? 'any',
    fact: entry.fact,
  }
}

function nextCustomId(entries: ForcedKnowledgeEntry[]): string {
  const used = new Set(entries.map((entry) => entry.id))
  let n = entries.length + 1
  while (used.has(`custom-${n}`)) n += 1
  return `custom-${n}`
}

/** 父组件每次打开时换 key 重新挂载，表单状态从 `draft` 初始化。 */
function KnowledgeDialog({
  draft,
  entries,
  onClose,
  onSave,
}: {
  draft: KnowledgeDraft | null
  entries: ForcedKnowledgeEntry[]
  onClose: () => void
  onSave: (entry: ForcedKnowledgeEntry, index: number | null) => void
}) {
  const [form, setForm] = useState<KnowledgeDraft>(() => draft ?? emptyKnowledgeDraft())
  const [submitted, setSubmitted] = useState(false)
  const set = <K extends keyof KnowledgeDraft>(key: K, value: KnowledgeDraft[K]) => setForm((prev) => ({ ...prev, [key]: value }))

  const topics = parseForcedKnowledgeTopics(form.topicsText)
  const id = form.id.trim()
  const generatedId = nextCustomId(entries)
  const duplicateId = Boolean(id) && entries.some((entry, index) => index !== form.index && entry.id === id)
  const topicsMissing = topics.length === 0
  const factMissing = !form.fact.trim()
  const isNew = form.index === null

  const submit = () => {
    setSubmitted(true)
    if (topicsMissing || factMissing || duplicateId) return
    const entry: ForcedKnowledgeEntry = { id: id || generatedId, enabled: form.enabled, topics, fact: form.fact.trim() }
    if (form.locale !== 'any') entry.locale = form.locale
    onSave(entry, form.index)
  }

  return (
    <ModalShell
      open={draft !== null}
      onClose={onClose}
      title={isNew ? '新增知识条目' : '编辑知识条目'}
      description="用户本轮自己写的文字包含任一关键词时，把内容作为当轮补充上下文提供给模型；每轮最多使用 3 条。"
      width="max-w-xl"
      footer={
        <>
          <Button variant="outline" size="sm" onClick={onClose}>
            取消
          </Button>
          <Button size="sm" onClick={submit}>
            {isNew ? '添加条目' : '更新条目'}
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <div className="grid gap-4 sm:grid-cols-[1fr_10rem]">
          <div className="space-y-1.5">
            <Label htmlFor="forced-knowledge-id">ID</Label>
            <Input
              id="forced-knowledge-id"
              className="font-mono text-xs"
              value={form.id}
              placeholder={isNew ? `留空自动生成：${generatedId}` : ''}
              onChange={(event) => set('id', event.target.value)}
            />
            {duplicateId && <p className="text-[11px] text-destructive">已有相同 ID 的条目</p>}
          </div>
          <div className="space-y-1.5">
            <Label>语言</Label>
            <Select value={form.locale} onValueChange={(value) => set('locale', value as LocaleChoice)}>
              <SelectTrigger size="sm" aria-label="语言">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="any">不限</SelectItem>
                <SelectItem value="en">英文（en）</SelectItem>
                <SelectItem value="zh">中文（zh）</SelectItem>
              </SelectContent>
            </Select>
          </div>
          <p className="text-[11px] leading-4 text-muted-foreground sm:col-span-2">
            中文条目只在用户用中文提问时使用；英文条目在用户用英文提问，或同一关键词没有中文条目时使用；不限语言的条目总是参与匹配。
          </p>
        </div>
        <div className="space-y-1.5">
          <Label htmlFor="forced-knowledge-topics">关键词</Label>
          <Input
            id="forced-knowledge-topics"
            value={form.topicsText}
            placeholder="claude models, 模型阵容, opus 5.5"
            onChange={(event) => set('topicsText', event.target.value)}
          />
          <p className="text-[11px] leading-4 text-muted-foreground">
            用逗号分隔，关键词里可以有空格；不区分大小写，用户文字包含任一关键词即命中。
            {topics.length > 0 ? `当前 ${topics.length} 个。` : ''}
          </p>
          {submitted && topicsMissing && <p className="text-[11px] text-destructive">至少填写一个关键词</p>}
        </div>
        <div className="space-y-1.5">
          <Label htmlFor="forced-knowledge-fact">内容</Label>
          <Textarea
            id="forced-knowledge-fact"
            rows={7}
            value={form.fact}
            placeholder="只写可以核查的公开事实。"
            onChange={(event) => set('fact', event.target.value)}
          />
          {submitted && factMissing && <p className="text-[11px] text-destructive">内容不能为空</p>}
        </div>
        <div className="flex items-center justify-between gap-4 rounded-md border bg-background px-3 py-2.5">
          <div>
            <div className="text-[13px] font-medium">启用</div>
            <div className="text-xs text-muted-foreground">停用的条目保留在列表里，但不参与匹配。</div>
          </div>
          <Switch checked={form.enabled} onCheckedChange={(value) => set('enabled', value)} aria-label="启用条目" />
        </div>
        <p className="text-xs leading-5 text-muted-foreground">条目改动在点击页面底部的“保存配置”后生效。</p>
      </div>
    </ModalShell>
  )
}

function KnowledgeTable({ entries, onChange }: { entries: ForcedKnowledgeEntry[]; onChange: (next: ForcedKnowledgeEntry[]) => void }) {
  const confirm = useConfirm()
  const [editing, setEditing] = useState<KnowledgeDraft | null>(null)
  const [dialogKey, setDialogKey] = useState(0)
  const [builtinOpen, setBuiltinOpen] = useState(false)
  const enabledCount = entries.filter((entry) => entry.enabled).length

  const openDialog = (draft: KnowledgeDraft) => {
    setDialogKey((key) => key + 1)
    setEditing(draft)
  }
  const save = (entry: ForcedKnowledgeEntry, index: number | null) => {
    onChange(index === null ? [...entries, entry] : entries.map((item, itemIndex) => (itemIndex === index ? entry : item)))
    setEditing(null)
  }
  const toggle = (index: number, enabled: boolean) =>
    onChange(entries.map((item, itemIndex) => (itemIndex === index ? { ...item, enabled } : item)))
  const remove = async (index: number) => {
    const entry = entries[index]
    if (!entry) return
    const name = entry.id || `第 ${index + 1} 条`
    const ok = await confirm({
      title: '删除知识条目？',
      message: entries.length === 1 ? `删除“${name}”后列表为空，保存配置后改用内置条目。` : `删除“${name}”，保存配置后生效。`,
      confirmText: '删除',
      tone: 'danger',
    })
    if (ok) onChange(entries.filter((_, itemIndex) => itemIndex !== index))
  }

  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="text-xs leading-5 text-muted-foreground">
          {entries.length === 0 ? '未配置条目，使用内置条目。' : `已配置 ${entries.length} 条，启用 ${enabledCount} 条。`}
        </div>
        <div className="flex gap-2">
          <Button size="sm" variant="outline" onClick={() => setBuiltinOpen(true)}>
     内置条目与预览
   </Button>
   <Button size="sm" variant="outline" onClick={() => openDialog(emptyKnowledgeDraft())}>
     <Plus />
     新增条目
          </Button>
        </div>
      </div>
      <BuiltinKnowledgeDialog
 open={builtinOpen}
 onClose={() => setBuiltinOpen(false)}
        configuredCount={entries.length}
        onCopyAll={(builtin) => {
   const taken = new Set(entries.map((entry) => entry.id))
          onChange([...entries, ...builtin.filter((entry) => !taken.has(entry.id))])
          setBuiltinOpen(false)
        }}
      />
      {entries.length > 0 && enabledCount === 0 && (
        <div className="text-xs leading-5 text-warning">所有条目都已停用：不会提供任何知识，也不会改用内置条目。</div>
      )}
      <div className="overflow-hidden rounded-lg border bg-card">
        <Table className="min-w-[720px]">
          <TableHeader>
            <TableRow>
              <TableHead className="w-16">启用</TableHead>
              <TableHead className="w-36">ID</TableHead>
              <TableHead className="w-56">关键词</TableHead>
              <TableHead className="w-16">语言</TableHead>
              <TableHead>内容</TableHead>
              <TableHead className="w-24 text-right">操作</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {entries.length === 0 ? (
              <TableRow>
                <TableCell colSpan={6} className="whitespace-normal py-6 text-center text-sm text-muted-foreground">
                  使用内置条目（Claude 模型阵容、思考参数、提示缓存、服务端工具等公开信息）。新增条目后只使用这里配置的条目。
                </TableCell>
              </TableRow>
            ) : (
              entries.map((entry, index) => (
                <TableRow key={`${entry.id}-${index}`}>
                  <TableCell>
                    <Switch checked={entry.enabled} onCheckedChange={(value) => toggle(index, value)} aria-label={`启用 ${entry.id}`} />
                  </TableCell>
                  <TableCell className="max-w-[9rem] truncate font-mono text-xs" title={entry.id}>
                    {entry.id || '-'}
                  </TableCell>
                  <TableCell className="max-w-[14rem] whitespace-normal">
                    <div className="line-clamp-2 text-xs" title={entry.topics.join(', ')}>
                      {entry.topics.join(', ')}
                    </div>
                  </TableCell>
                  <TableCell className="text-xs text-muted-foreground">{LOCALE_LABEL[entry.locale ?? 'any']}</TableCell>
                  <TableCell className="max-w-[24rem] whitespace-normal">
                    <div className="line-clamp-2 text-xs text-muted-foreground" title={entry.fact}>
                      {entry.fact}
                    </div>
                  </TableCell>
                  <TableCell className="text-right">
                    <div className="flex justify-end gap-1">
                      <Button
                        variant="ghost"
                        size="icon-xs"
                        title="编辑"
                        aria-label="编辑"
                        onClick={() => openDialog(draftFromEntry(entry, index))}
                      >
                        <Pencil />
                      </Button>
                      <DeleteButton title="删除" size="icon-xs" onClick={() => void remove(index)} />
                    </div>
                  </TableCell>
                </TableRow>
              ))
            )}
          </TableBody>
        </Table>
      </div>
      <KnowledgeDialog key={dialogKey} draft={editing} entries={entries} onClose={() => setEditing(null)} onSave={save} />
    </div>
  )
}

// ─── 配置区 ──────────────────────────────────────────────────────────────────

export function ForcedModelMappingSection({
  mapping,
  capabilities,
  routes,
  onChange,
}: {
  mapping: ForcedModelMappingConfig
  capabilities?: ModelCapabilitiesStatus
  /** 已知路由，用于路由覆盖的快捷选择。 */
  routes: string[]
  onChange: (next: ForcedModelMappingConfig) => void
}) {
  const catalog = useForcedModelCatalog()
  const options = useMemo(() => catalogOptions(capabilities, catalog.data), [capabilities, catalog.data])
  const active = forcedModelMappingAnyRouteEnabled(mapping)
  const targetModels = useMemo(
    () => [...mapping.rules.map((rule) => rule.target), ...Object.values(mapping.modelOverrides), mapping.defaultTarget],
    [mapping.rules, mapping.modelOverrides, mapping.defaultTarget],
  )
  const targetStatus = useForcedModelTargets(targetModels)
  const catalogEmpty = !catalog.data?.kiro?.length && (!capabilities || capabilities.models.length === 0)
  const limits = FORCED_CONTEXT_BACKOFF_LIMITS
  const setBackoff =
    <K extends keyof ForcedContextBackoffConfig>(key: K) =>
    (value: ForcedContextBackoffConfig[K]) =>
      onChange({ ...mapping, contextBackoff: { ...mapping.contextBackoff, [key]: value } })
  const restoreDefaultRules = () => {
    const defaults = defaultForcedModelMapping()
    onChange({
      ...mapping,
      rules: defaults.rules,
      modelOverrides: defaults.modelOverrides,
      defaultTarget: defaults.defaultTarget,
    })
  }

  const applyGlobalTarget = (target: string) =>
    onChange({ ...mapping, rules: [], modelOverrides: {}, defaultTarget: target })

  const targetSummary = targetStatus.data?.length ? (
    <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
      {targetStatus.data.map((target) => (
        <span key={target.model}>
          <span className="font-mono">{target.model}</span>
          {target.inCatalog ? (
            <span className={target.enabledAccounts > 0 ? undefined : 'text-destructive'}>
              ：可承接的启用账号 {target.enabledAccounts} 个
            </span>
          ) : (
            <span className="text-warning">：不在上游模型目录中</span>
          )}
        </span>
      ))}
    </div>
  ) : null

  return (
    <div className="space-y-6">
      <div className="space-y-4">
        <div className="max-w-3xl space-y-1 text-xs leading-5 text-muted-foreground">
          <p>开启后，所有 Claude 模型请求按下面的规则改发到目标模型，对外仍显示客户端请求的模型。</p>
          <p>
            它和“模型解析”里的模型映射、跨族兜底无关：模型映射只负责把模型名解析成上游认识的写法，强制收敛是在入口把请求换成目标模型，两者各自独立。外部号池不受影响，仍按客户端请求的模型发送。
          </p>
        </div>
        <TogField
          label="启用强制模型收敛"
          desc={`所有路由的默认开关；在“路由覆盖”里单独设置过的路由以覆盖为准。当前：${routeStatusSummary(mapping)}。`}
          checked={mapping.enabled}
          onChange={(enabled) => onChange({ ...mapping, enabled })}
        />
        {active && catalogEmpty && (
          <div className="text-xs leading-5 text-warning">
            上游模型目录还没有同步，保存时无法校验目标模型是否可用，建议先到模型页同步目录。
          </div>
        )}
      </div>

      <SubBlock
        title="单模型覆盖"
        desc="按客户端请求的完整模型 ID 单独指定目标，优先级高于模型族规则。模型名支持点号和横杠写法，保存时统一为小写横杠。"
      >
        <ModelOverridesTable
          overrides={mapping.modelOverrides}
          options={options}
          onChange={(modelOverrides) => onChange({ ...mapping, modelOverrides })}
        />
      </SubBlock>

      <SubBlock
        title="收敛规则"
        desc="精确模型覆盖优先，然后按模型族规则匹配，最后使用默认目标。目标模型可以从 Claude 官方目录、Kiro 上游目录选择，也可以直接输入自定义模型名。"
        actions={
          <>
            <Button size="sm" variant="outline" title="清空规则和单模型覆盖，全部映射到 Haiku 4.5" onClick={() => applyGlobalTarget(DEFAULT_FORCED_HAIKU_TARGET)}>
              <Zap />
              全部 → Haiku 4.5
            </Button>
            <Button size="sm" variant="outline" title="清空规则和单模型覆盖，全部映射到 Sonnet 4.5" onClick={() => applyGlobalTarget(DEFAULT_FORCED_SONNET_TARGET)}>
              <Zap />
              全部 → Sonnet 4.5
            </Button>
            <Button
              size="sm"
              variant="outline"
              onClick={() => onChange({ ...mapping, rules: [...mapping.rules, { families: [], target: '' }] })}
            >
              <Plus />
              添加规则
            </Button>
            <Button size="sm" variant="outline" title="恢复默认规则和默认目标模型" onClick={restoreDefaultRules}>
              <RotateCcw />
              恢复默认
            </Button>
          </>
        }
      >
        <RulesTable rules={mapping.rules} options={options} onChange={(rules) => onChange({ ...mapping, rules })} />
        {targetSummary}
        <div className="max-w-xl space-y-1.5 pt-1">
          <div className="text-[13px] font-medium">默认目标模型</div>
          <TargetModelInput
            value={mapping.defaultTarget}
            options={options}
            ariaLabel="默认目标模型"
            onChange={(defaultTarget) => onChange({ ...mapping, defaultTarget })}
          />
          <div className="text-xs leading-5 text-muted-foreground">
            规则都没有命中的 Claude 模型（包括 opus、sonnet 这类短别名）改发到这里。
          </div>
        </div>
      </SubBlock>

      <SubBlock
        title="路由覆盖"
        desc="按路由单独开启或关闭强制收敛。多个前缀都匹配时取最长的那个；没有覆盖的路由跟随总开关；“/” 匹配所有路由。"
      >
        <RouteOverridesTable mapping={mapping} routes={routes} onChange={onChange} />
      </SubBlock>

      <SubBlock title="响应结构">
        <TogField
          label="工具间文字作为思考块"
   desc="外显模型为 Fable 全系、Mythos 5.1/5.2、Opus 5.5 或 Sonnet 5.5 时，紧挨在工具调用之前的文字作为 thinking 块下发（思考隐藏时内容为空，display 为 updates 时保留文字），和官方模型返回的结构一致；最后没有工具调用的回答照常下发。默认开启。"
          checked={mapping.interleavedTextAsThinking}
          onChange={(interleavedTextAsThinking) => onChange({ ...mapping, interleavedTextAsThinking })}
        />
      </SubBlock>

      <SubBlock
        title="上下文退避"
 desc="客户端按外显模型的窗口（多为 1M）发送历史。超出目标模型窗口时，先把早期对话浓缩成一段工作记录（摘要检查点），放不下再依次去掉旧的思考块、清理旧的工具结果、从最早的轮次删除历史、截断最大的单个内容块。窗口取目标模型在模型目录里的输入上限，没有时按 200K。强制收敛开启时，没有显式配置的路由会默认使用模型驱动的 WebSearch。"
      >
 <div className="grid gap-x-6 gap-y-5 md:grid-cols-2">
          <TogField
     label="摘要检查点"
     desc="超出预算时用目标模型把早期对话浓缩成工作记录，接在首条用户消息后面，保留目标、约定和进度；关闭时只做裁剪。摘要只计入内部成本，不进用户 usage。默认开启。"
     checked={mapping.contextBackoff.summaryEnabled}
     onChange={(summaryEnabled) => onChange({ ...mapping, contextBackoff: { ...mapping.contextBackoff, summaryEnabled } })}
   />
   <NumField
     label="摘要等待上限"
     desc={`同步等待摘要的最长时间，超时这一轮先裁剪发出，摘要在后台继续完成供下一轮使用。范围 ${limits.summaryTimeoutSecs.min}–${limits.summaryTimeoutSecs.max}，默认 120。`}
     value={mapping.contextBackoff.summaryTimeoutSecs}
     min={limits.summaryTimeoutSecs.min}
     max={limits.summaryTimeoutSecs.max}
     suffix="秒"
     onChange={setBackoff('summaryTimeoutSecs')}
          />
          <NumField
     label="预压缩比例"
     desc={`估算超过 预算 × 该比例 时在后台预先生成下一个检查点，本次照常发送。范围 ${limits.summaryPrecompactRatio.min}–${limits.summaryPrecompactRatio.max}，默认 0.75。`}
            value={mapping.contextBackoff.summaryPrecompactRatio}
     min={limits.summaryPrecompactRatio.min}
     max={limits.summaryPrecompactRatio.max}
     step={0.05}
     suffix="比例"
     onChange={setBackoff('summaryPrecompactRatio')}
   />
   <NumField
     label="压缩后目标比例"
     desc={`新检查点让 首条消息 + 摘要 + 保留的尾部 落到 预算 × 该比例 以下。范围 ${limits.summaryTargetRatio.min}–${limits.summaryTargetRatio.max}，默认 0.45。`}
     value={mapping.contextBackoff.summaryTargetRatio}
     min={limits.summaryTargetRatio.min}
     max={limits.summaryTargetRatio.max}
     step={0.05}
     suffix="比例"
     onChange={setBackoff('summaryTargetRatio')}
   />
   <NumField
     label="预留输出上限"
            desc={`预留输出取客户端的 max_tokens，最多这个值。上游不接收 max_tokens，实际输出很少超过 32K，预留太多会挤占输入。范围 ${limits.maxReservedOutputTokens.min}–${limits.maxReservedOutputTokens.max}，默认 32000。`}
     value={mapping.contextBackoff.maxReservedOutputTokens}
     min={limits.maxReservedOutputTokens.min}
     max={limits.maxReservedOutputTokens.max}
     step={1000}
     suffix="tokens"
     onChange={setBackoff('maxReservedOutputTokens')}
   />
          <NumField
            label="安全余量比例"
            desc={`输入预算 = 窗口 − 预留输出 − 安全余量，安全余量按剩余空间的比例留出，用来抵消本地估算和上游计数的偏差。范围 ${limits.safetyMarginRatio.min}–${limits.safetyMarginRatio.max}，默认 0.05。`}
            value={mapping.contextBackoff.safetyMarginRatio}
            min={limits.safetyMarginRatio.min}
            max={limits.safetyMarginRatio.max}
            step={0.01}
            suffix="比例"
            onChange={setBackoff('safetyMarginRatio')}
          />
          <NumField
            label="预留输出下限"
     desc={`预留输出不低于这个值。范围 ${limits.minReservedOutputTokens.min}–${limits.minReservedOutputTokens.max}，默认 8000。`}
            value={mapping.contextBackoff.minReservedOutputTokens}
            min={limits.minReservedOutputTokens.min}
            max={limits.minReservedOutputTokens.max}
            suffix="tokens"
            onChange={setBackoff('minReservedOutputTokens')}
          />
          <NumField
            label="保留最近工具结果"
            desc={`清理旧工具结果时，最近这些条保留原文，更早的换成占位文字，tool_use 和 tool_result 的配对不变。范围 ${limits.keepRecentToolResults.min}–${limits.keepRecentToolResults.max}，默认 8。`}
            value={mapping.contextBackoff.keepRecentToolResults}
            min={limits.keepRecentToolResults.min}
            max={limits.keepRecentToolResults.max}
            suffix="条"
            onChange={setBackoff('keepRecentToolResults')}
          />
          <NumField
            label="清理步长"
            desc={`清理边界每次按这个步长推进；历史增长不到一个步长时，处理后的前缀保持不变，提示缓存照常命中。范围 ${limits.stepTokens.min}–${limits.stepTokens.max}，默认 50000。`}
            value={mapping.contextBackoff.stepTokens}
            min={limits.stepTokens.min}
            max={limits.stepTokens.max}
            step={1000}
            suffix="tokens"
            onChange={setBackoff('stepTokens')}
          />
        </div>
      </SubBlock>

      <SubBlock
        title="知识条目"
        desc="用户问到 Claude 模型、参数等话题时提供给模型的补充事实，只匹配用户本轮自己写的文字。列表为空时使用内置条目；配置了任意条目后只使用这里的条目。"
      >
        <KnowledgeTable entries={mapping.knowledge} onChange={(knowledge) => onChange({ ...mapping, knowledge })} />
      </SubBlock>
    </div>
  )
}

/** 内置知识条目和预览：用一段用户文字看会命中哪些条目。 */
function BuiltinKnowledgeDialog({
  open,
  onClose,
  configuredCount,
  onCopyAll,
}: {
  open: boolean
  onClose: () => void
  configuredCount: number
  onCopyAll: (entries: ForcedKnowledgeEntry[]) => void
}) {
  const [text, setText] = useState('')
  const [submitted, setSubmitted] = useState('')
  const builtin = useForcedModelKnowledge(open, '')
  const preview = useForcedModelKnowledge(open && submitted.trim().length > 0, submitted)
  const entries = builtin.data?.builtin ?? []
  return (
    <ModalShell
      open={open}
      onClose={onClose}
      title="内置知识条目"
      description="配置列表为空时使用这些条目。可以一键复制到配置后再修改；配置了任意条目后，只使用配置里的条目。"
      width="max-w-3xl"
      footer={
        <>
          <Button variant="outline" size="sm" onClick={onClose}>
     关闭
   </Button>
   <Button size="sm" disabled={entries.length === 0} onClick={() => onCopyAll(entries)}>
     {configuredCount > 0 ? '追加到配置' : '复制到配置'}
   </Button>
        </>
      }
    >
      <div className="space-y-4">
 <div className="space-y-2">
   <Label htmlFor="knowledge-preview">命中预览</Label>
          <div className="flex gap-2">
     <Input
       id="knowledge-preview"
       value={text}
       placeholder="输入一段用户可能问的话，例如：现在有哪些 Claude 模型？"
       onChange={(event) => setText(event.target.value)}
       onKeyDown={(event) => {
  if (event.key === 'Enter') setSubmitted(text)
       }}
     />
     <Button size="sm" variant="outline" onClick={() => setSubmitted(text)}>
       预览
     </Button>
          </div>
   {submitted.trim() && (
     <div className="rounded-md border border-border/70 p-3 text-xs leading-5">
       {preview.isLoading ? (
  <span className="text-muted-foreground">加载中…</span>
) : (preview.data?.preview?.facts ?? []).length === 0 ? (
         <span className="text-muted-foreground">没有命中任何条目，这句话不会附带补充事实。</span>
) : (
  <ol className="list-decimal space-y-1 pl-4">
           {(preview.data?.preview?.facts ?? []).map((fact, index) => (
      <li key={index}>{fact}</li>
    ))}
  </ol>
       )}
     </div>
   )}
        </div>
 <Table>
          <TableHeader>
     <TableRow>
       <TableHead className="w-36">ID</TableHead>
       <TableHead className="w-14">语言</TableHead>
       <TableHead>关键词 / 内容</TableHead>
            </TableRow>
   </TableHeader>
          <TableBody>
     {entries.map((entry) => (
       <TableRow key={entry.id}>
         <TableCell className="align-top whitespace-normal break-all font-mono text-xs">{entry.id}</TableCell>
         <TableCell className="align-top text-xs">{entry.locale ?? '不限'}</TableCell>
  <TableCell className="space-y-1 whitespace-normal break-words text-xs leading-5">
           <div className="text-muted-foreground">{entry.topics.join('、')}</div>
           <div>{entry.fact}</div>
         </TableCell>
       </TableRow>
     ))}
   </TableBody>
        </Table>
      </div>
    </ModalShell>
  )
}
