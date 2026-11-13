// Seeds OAuth fixture credentials straight into a caller-owned PostgreSQL database before the
// service starts.
//
// The service no longer reads a credentials file at startup, and the Admin API refreshes every
// OAuth credential against the fixed Kiro auth host before storing it, so fixtures that only carry
// an access token cannot go through it without reaching a real upstream. This writes the same
// rows the removed file bootstrap wrote: one `credentials` row per fixture with revision 1 and no
// runtime state row, then advances `credentials_id_seq` past the seeded ids.
import crypto from 'node:crypto'
import { spawnSync } from 'node:child_process'

function sha256Hex(value) {
  return crypto.createHash('sha256').update(value).digest('hex')
}

function sqlLiteral(value) {
  if (value === null || value === undefined) return 'NULL'
  return `'${String(value).replaceAll("'", "''")}'`
}

function compactAuthValue(value) {
  return String(value || '').replace(/[^A-Za-z0-9]/g, '').toLowerCase()
}

function credentialRowSql(credential, id) {
  if (!Number.isSafeInteger(id) || id < 1) {
    throw new Error('seeded fixture credential ids must be positive integers')
  }
  const isApiKey = Boolean(credential.kiroApiKey) || compactAuthValue(credential.authMethod) === 'apikey'
  const authKind = isApiKey
    ? 'api_key'
    : String(credential.authMethod || 'oauth').trim().toLowerCase() || 'oauth'
  const apiKeyHash = isApiKey && credential.kiroApiKey ? sha256Hex(credential.kiroApiKey) : null
  const refreshTokenHash = !isApiKey && credential.refreshToken ? sha256Hex(credential.refreshToken) : null
  return `INSERT INTO credentials (
  id, priority, disabled, auth_kind, api_key_hash, refresh_token_hash, data, updated_at, deleted_at, revision
) VALUES (
  ${id}, ${Number(credential.priority || 0)}, ${credential.disabled ? 'true' : 'false'},
  ${sqlLiteral(authKind)}, ${sqlLiteral(apiKeyHash)}, ${sqlLiteral(refreshTokenHash)},
  ${sqlLiteral(JSON.stringify({ ...credential, id }))}::jsonb, now(), NULL, 1
);`
}

function postgresCommandEnvironment(postgresUrl, baseEnvironment) {
  const parsed = new URL(postgresUrl)
  const environment = {
    ...baseEnvironment,
    PGHOST: parsed.hostname.replace(/^\[|\]$/g, ''),
    PGPORT: parsed.port || '5432',
    PGDATABASE: decodeURIComponent(parsed.pathname.replace(/^\//, '')),
    PGUSER: decodeURIComponent(parsed.username || ''),
    PGCONNECT_TIMEOUT: '5',
    PGAPPNAME: 'kiro-validation-credential-seed',
  }
  if (parsed.password) environment.PGPASSWORD = decodeURIComponent(parsed.password)
  const sslMode = parsed.searchParams.get('sslmode')
  if (sslMode) environment.PGSSLMODE = sslMode
  return environment
}

function runChecked(command, args, options, label) {
  const result = spawnSync(command, args, { encoding: 'utf8', maxBuffer: 8 * 1024 * 1024, ...options })
  if (result.error) throw new Error(`${label} failed to start: ${result.error.message}`)
  if (result.status !== 0) {
    throw new Error(`${label} failed (${result.status}): ${String(result.stderr || '').trim().slice(0, 2000)}`)
  }
}

/**
 * @param {object} options
 * @param {string} options.binary frozen kiro-rs binary; `maintenance migrate` creates the schema
 * @param {string} options.configPath service config used for the migration
 * @param {object} options.serviceEnvironment environment the service itself is started with
 * @param {string} options.postgresUrl database the service is configured with
 * @param {object[]} options.credentials fixtures; one without `id` gets its 1-based position, the
 *   id the file bootstrap assigned on an empty database
 * @param {string} options.cwd working directory for both commands
 */
export function seedCredentialsBeforeStartup({
  binary,
  configPath,
  serviceEnvironment,
  postgresUrl,
  credentials,
  cwd,
}) {
  if (credentials.length === 0) return
  runChecked(binary, ['--config', configPath, 'maintenance', 'migrate'], {
    cwd,
    env: serviceEnvironment,
  }, 'kiro-rs maintenance migrate')
  const ids = credentials.map((credential, index) => Number(credential.id ?? index + 1))
  const sql = [
    'BEGIN;',
    ...credentials.map((credential, index) => credentialRowSql(credential, ids[index])),
    `SELECT setval('credentials_id_seq', GREATEST((SELECT last_value FROM credentials_id_seq), ${Math.max(...ids)}), true);`,
    'COMMIT;',
  ].join('\n')
  const psql = process.env.KIRO_PSQL_BINARY || 'psql'
  runChecked(psql, ['-X', '-q', '-v', 'ON_ERROR_STOP=1', '-f', '-'], {
    cwd,
    input: sql,
    env: postgresCommandEnvironment(postgresUrl, { PATH: serviceEnvironment.PATH || process.env.PATH || '' }),
  }, 'psql credential seed')
}
