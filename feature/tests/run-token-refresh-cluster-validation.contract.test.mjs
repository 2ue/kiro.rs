import assert from 'node:assert/strict'
import fs from 'node:fs'
import path from 'node:path'
import test from 'node:test'

const ROOT = fs.realpathSync(path.resolve(import.meta.dirname, '../..'))
const RUNNER = path.join(ROOT, 'feature/tests/run-token-refresh-cluster-validation.mjs')
const source = fs.readFileSync(RUNNER, 'utf8')

test('refresh cluster runner is explicit about both isolated stores', () => {
  assert.match(source, /KIRO_TOKEN_REFRESH_CLUSTER_REDIS_URL/)
  assert.match(source, /KIRO_TOKEN_REFRESH_CLUSTER_POSTGRES_URL/)
  assert.match(source, /KIRO_RS_TEST_REDIS_ISOLATED=1/)
  assert.match(source, /KIRO_RS_TEST_POSTGRES_ISOLATED=1/)
  assert.match(source, /nonzero database in 1\.\.15/)
  assert.match(source, /PostgreSQL URL must target loopback/)
})

test('refresh cluster runner refuses protected port and Docker', () => {
  assert.match(source, /port 9022 is protected/)
  assert.doesNotMatch(source, /docker\s+(?:compose|run|exec|start|stop|rm)/i)
  assert.doesNotMatch(source, /9022[^\n]*(?:lsof|netstat|ss)|(?:lsof|netstat|ss)[^\n]*9022/i)
})

test('refresh cluster runner delegates every Cargo command through scoped wrapper', () => {
  assert.match(source, /feature\/tests\/run-cargo-scoped\.sh/)
  assert.match(source, /RUSTUP_TOOLCHAIN=1\.92\.0/)
  assert.match(source, /cargo fmt --all -- --check/)
  assert.match(source, /git diff --check/)
  assert.doesNotMatch(source, /target\/(?:debug|release)/)
})

test('refresh cluster matrix names all required two-instance contracts', () => {
  for (const name of [
    'token_refresh_two_manager_rotating_and_non_rotating_share_one_send_and_pg_authority_for_five_rounds',
    'token_refresh_two_manager_pg_cas_fences_stale_rotating_and_non_rotating_results_for_five_rounds',
    'token_refresh_two_manager_failure_replay_and_cancelled_leader_recover_without_send_amplification_for_five_rounds',
    'token_refresh_two_manager_cancelled_health_claim_is_reclaimed_once_for_five_rounds',
  ]) {
    assert.match(source, new RegExp(name))
  }
  assert.match(source, /token_refresh_redis_stale_leader_cannot_overwrite_success/)
  assert.match(source, /token_refresh_redis_failure_replay_health_claim_and_identity_are_fenced/)
})

test('refresh cluster runner refuses dirty Redis and never flushes the database', () => {
  assert.match(source, /const before = await redisCommand\(redisTarget, \['DBSIZE'\]\)/)
  assert.match(source, /if \(before !== 0\) throw new Error\(`isolated Redis database/)
  assert.match(source, /redisWasEmpty = true/)
  assert.match(source, /residualKeyCount/)
  assert.doesNotMatch(source, /\bFLUSH(?:DB|ALL)\b/)
  assert.match(source, /tempRoot.*fs\.rmSync/)
})
