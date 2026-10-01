import { useEffect, useState } from 'react'
import { ChevronDown } from 'lucide-react'
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '@/components/ui/collapsible'
import { Input } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'
import { ListInput, NumberInput, SelectControl, SettingRow, ToggleControl } from '@/components/patterns/fields'
import { deepEqual } from '@/domain/config-diff'
import type { FieldDef, FieldGroup } from './registry'
import type { ConfigDraft } from './use-config-draft'

function parseCodes(raw: string): number[] {
  const seen = new Set<number>()
  return raw
    .split(/[\s,，;；]+/)
    .map((v) => Number(v.trim()))
    .filter((c) => Number.isInteger(c) && c >= 100 && c <= 599 && !seen.has(c) && (seen.add(c), true))
}

function TextField({ value, onCommit, multiline, disabled }: { value: string; onCommit: (v: string) => void; multiline?: boolean; disabled?: boolean }) {
  const [text, setText] = useState(value)
  useEffect(() => setText(value), [value])
  const commit = () => text !== value && onCommit(text)
  return multiline ? (
    <Textarea rows={5} value={text} disabled={disabled} onChange={(e) => setText(e.target.value)} onBlur={commit} className="font-mono text-xs" />
  ) : (
    <Input value={text} disabled={disabled} onChange={(e) => setText(e.target.value)} onBlur={commit} className="font-mono text-xs" />
  )
}

export function FieldControl({ field, cfg }: { field: FieldDef; cfg: ConfigDraft }) {
  const value = cfg.get(field.path)
  const disabled = field.disabledWhen?.((p) => cfg.get(p)) ?? false
  const dirty = !deepEqual(value, cfg.baseValue(field.path))
  const id = `f-${field.path}`
  const stacked = field.kind === 'list' || field.kind === 'codes' || (field.kind === 'text' && field.multiline)
  const set = (v: unknown) => cfg.set(field.path, v)

  let control: React.ReactNode
  switch (field.kind) {
    case 'number':
      control = <NumberInput id={id} value={value as number} min={field.min} max={field.max} step={field.step} suffix={field.suffix} disabled={disabled} onChange={(v) => set(v ?? field.min ?? 0)} />
      break
    case 'bool':
      control = <ToggleControl id={id} checked={!!value} disabled={disabled} onChange={set} />
      break
    case 'select':
      control = <SelectControl id={id} value={String(value ?? field.options[0]?.value ?? '')} options={field.options} disabled={disabled} onChange={set} />
      break
    case 'list':
      control = <ListInput id={id} value={(value as string[]) ?? []} disabled={disabled} onChange={set} rows={3} />
      break
    case 'codes':
      control = <TextField value={((value as number[]) ?? []).join(', ')} disabled={disabled} onCommit={(t) => set(parseCodes(t))} />
      break
    case 'text':
      control = <TextField value={String(value ?? '')} multiline={field.multiline} disabled={disabled} onCommit={set} />
      break
  }
  return (
    <SettingRow
      htmlFor={id}
      label={field.label}
      description={field.desc}
      dirty={dirty}
      onReset={() => cfg.reset(field.path)}
      disabled={disabled}
      stacked={stacked}
    >
      {control}
    </SettingRow>
  )
}

export function GroupCard({ group, cfg, filter }: { group: FieldGroup; cfg: ConfigDraft; filter?: string }) {
  const match = (f: FieldDef) => !filter || `${f.label} ${f.desc ?? ''} ${f.path}`.toLowerCase().includes(filter.toLowerCase())
  const basic = group.fields.filter((f) => !f.advanced && match(f))
  const advanced = group.fields.filter((f) => f.advanced && match(f))
  if (!basic.length && !advanced.length) return null
  const advancedDirty = advanced.some((f) => !deepEqual(cfg.get(f.path), cfg.baseValue(f.path)))
  return (
    <section className="rounded-xl border bg-card">
      <header className="border-b px-4 py-3">
        <h3 className="text-sm font-semibold">{group.title}</h3>
        {group.description && <p className="mt-0.5 text-xs text-muted-foreground">{group.description}</p>}
      </header>
      <div className="divide-y px-4">
        {basic.map((f) => (
          <FieldControl key={f.path} field={f} cfg={cfg} />
        ))}
      </div>
      {advanced.length > 0 && (
        <Collapsible defaultOpen={!!filter || advancedDirty} className="border-t">
          <CollapsibleTrigger className="group flex w-full items-center gap-1 px-4 py-2.5 text-xs font-medium text-muted-foreground hover:text-foreground">
            <ChevronDown className="size-3.5 transition-transform group-data-[state=open]:rotate-180" />
            高级（{advanced.length}）{advancedDirty && <span className="ml-1 size-1.5 rounded-full bg-primary" />}
          </CollapsibleTrigger>
          <CollapsibleContent className="divide-y border-t px-4">
            {advanced.map((f) => (
              <FieldControl key={f.path} field={f} cfg={cfg} />
            ))}
          </CollapsibleContent>
        </Collapsible>
      )}
    </section>
  )
}
