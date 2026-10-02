import type { AddCredentialRequest, CredentialValidationItem } from '@/api/types'
import { Checkbox } from '@/components/ui/checkbox'
import { ToneBadge } from '@/components/status/tone-badge'
import { authMethodLabel, SUBSCRIPTION_LABEL, subscriptionTier } from '@/domain/labels'
import { translateError } from '@/domain/upstream-error'

export interface Candidate {
  credential: AddCredentialRequest
  include: boolean
  check?: CredentialValidationItem
}

function credentialHint(c: AddCredentialRequest): string {
  if (c.email) return c.email
  if (c.kiroApiKey) return `${c.kiroApiKey.slice(0, 8)}…${c.kiroApiKey.slice(-4)}`
  if (c.refreshToken) return `refresh …${c.refreshToken.slice(-8)}`
  return '未知'
}

/** 预检步骤的候选账号列表：勾选、订阅档位、预检结果 */
export function CandidateList({ candidates, onToggle }: { candidates: Candidate[]; onToggle: (index: number, include: boolean) => void }) {
  return (
    <div className="divide-y rounded-lg border">
      {candidates.map((c, i) => {
        const tier = subscriptionTier(c.check?.subscriptionTitle)
        return (
          <label key={i} className="flex cursor-pointer items-center gap-3 px-3 py-2 text-sm hover:bg-muted/40">
            <Checkbox checked={c.include} onCheckedChange={(v) => onToggle(i, v === true)} />
            <span className="num w-8 text-xs text-muted-foreground">{i + 1}</span>
            <span className="min-w-0 flex-1 truncate">{credentialHint(c.credential)}</span>
            <span className="text-xs text-muted-foreground">{authMethodLabel(c.credential.authMethod)}</span>
            {c.check ? (
              c.check.matchedExistingCredentialId ? (
                <ToneBadge tone="neutral">已存在 #{c.check.matchedExistingCredentialId}</ToneBadge>
              ) : c.check.ok ? (
                <ToneBadge tone="success">{c.check.subscriptionTitle ? SUBSCRIPTION_LABEL[tier] : '可用'}</ToneBadge>
              ) : (
                <ToneBadge tone="danger" title={c.check.error ?? undefined}>
                  {c.check.error ? translateError(c.check.error)?.title : '失败'}
                </ToneBadge>
              )
            ) : (
              <ToneBadge tone="neutral">未预检</ToneBadge>
            )}
          </label>
        )
      })}
    </div>
  )
}
