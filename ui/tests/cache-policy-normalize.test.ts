// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { defaultCachePolicy, normalizeCachePolicy } from '../src/lib/runtime-config-defaults.ts'
import type { CacheRoutePolicyPatch } from '../src/types/api.ts'

const withPaths = (pathOverrides: Record<string, CacheRoutePolicyPatch>) =>
  normalizeCachePolicy({ ...defaultCachePolicy(), pathOverrides }).pathOverrides

test('只配了缓存写入时长或零用量估算的路径不会被当成空补丁丢掉', () => {
  const cacheTtlOnly = { cacheTtl: { local: 'force_1h' as const } }
  const zeroUsageOnly = { zeroUsageRepair: { output: { fallback: 'range' as const } } }
  assert.deepEqual(withPaths({ '/dfcache/ttl': cacheTtlOnly, '/dfcache/zero': zeroUsageOnly }), {
    '/dfcache/ttl': cacheTtlOnly,
    '/dfcache/zero': zeroUsageOnly,
  })
})

test('真正的空补丁仍然丢掉', () => {
  assert.deepEqual(withPaths({ '/dfcache/empty': {} }), {})
})
