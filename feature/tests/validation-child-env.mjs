const DEFAULT_PATH = '/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin'

const SAFE_ENV_NAMES = [
  'PATH',
  'TMPDIR',
  'TMP',
  'TEMP',
  'LANG',
  'LC_ALL',
  'LC_CTYPE',
  'TZ',
  'USER',
  'LOGNAME',
  'HOME',
  'VOLTA_HOME',
  'CARGO_HOME',
  'RUSTUP_HOME',
]

export function validationChildEnvironment(extra = {}) {
  const environment = {}
  for (const name of SAFE_ENV_NAMES) {
    if (typeof process.env[name] === 'string' && process.env[name] !== '') {
      environment[name] = process.env[name]
    }
  }
  if (!environment.PATH) environment.PATH = DEFAULT_PATH
  return { ...environment, ...extra }
}
