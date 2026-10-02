import { expect, it } from 'vitest'
import { parseCredentialImportEntries } from '@/lib/credential-import'

it('keeps KAM source status and dedupes', () => {
  const text = JSON.stringify({
    accounts: [
      { email: 'a@x.com', status: 'error', credentials: { refreshToken: 'r1' } },
      { email: 'b@x.com', status: 'active', credentials: { refreshToken: 'r2' } },
      { email: 'a@x.com', credentials: { refreshToken: 'r1' } },
    ],
  })
  const { entries, duplicates } = parseCredentialImportEntries(text)
  expect(entries.map((e) => [e.credential.email, e.sourceStatus])).toEqual([
    ['a@x.com', 'error'],
    ['b@x.com', 'active'],
  ])
  expect(duplicates).toBe(1)
})
