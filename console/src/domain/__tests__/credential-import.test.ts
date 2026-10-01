import { describe, expect, it } from 'vitest'
import { dedupeCredentials, parseCredentialImportText } from '@/lib/credential-import'

describe('credential import', () => {
  it('parses plain ksk keys with region', () => {
    const r = parseCredentialImportText('ksk_abc|eu-central-1\nksk_def')
    expect(r).toHaveLength(2)
    expect(r[0]).toMatchObject({ authMethod: 'api_key', kiroApiKey: 'ksk_abc', apiRegion: 'eu-central-1' })
  })

  it('parses snake_case JSON array and infers idc', () => {
    const r = parseCredentialImportText(JSON.stringify([{ refresh_token: 'rt1', client_id: 'c', client_secret: 's' }]))
    expect(r[0]).toMatchObject({ authMethod: 'idc', refreshToken: 'rt1', clientId: 'c', clientSecret: 's' })
  })

  it('parses KAM export with nested credentials', () => {
    const r = parseCredentialImportText(
      JSON.stringify({ accounts: [{ email: 'a@x.com', tags: ['t1'], credentials: { refreshToken: 'rt', profileArn: 'arn:aws:codewhisperer:us-east-1:1:profile/x' } }] }),
    )
    expect(r[0]).toMatchObject({ email: 'a@x.com', refreshToken: 'rt', apiRegion: 'us-east-1', tags: ['t1'] })
  })

  it('parses JSONL and skips items without tokens', () => {
    const r = parseCredentialImportText('{"refreshToken":"a"}\n{"email":"no-token"}')
    expect(r).toHaveLength(1)
  })

  it('dedupes by token', () => {
    const { unique, duplicates } = dedupeCredentials(parseCredentialImportText('[{"refreshToken":"a"},{"refreshToken":"a"},{"refreshToken":"b"}]'))
    expect(unique).toHaveLength(2)
    expect(duplicates).toBe(1)
  })
})
