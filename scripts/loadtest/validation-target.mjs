const LOOPBACK_HOSTS = new Set(['127.0.0.1', 'localhost', '::1', '[::1]'])

function explicitBoolean(value) {
  return value === true || value === 'true' || value === '1' || value === 'yes'
}

export function resolveLoadTarget(args, env = process.env) {
  const configuredBaseUrl = args.baseUrl || env.KIRO_BASE_URL
  if (!configuredBaseUrl) {
    throw new Error('an explicit --base-url or KIRO_BASE_URL is required')
  }

  let baseUrl
  try {
    baseUrl = new URL(configuredBaseUrl)
  } catch {
    throw new Error('the configured load-test base URL is invalid')
  }
  if (!['http:', 'https:'].includes(baseUrl.protocol)) {
    throw new Error('the load-test base URL must use http or https')
  }

  const loopback = LOOPBACK_HOSTS.has(baseUrl.hostname.toLowerCase())
  const effectivePort = Number.parseInt(
    baseUrl.port || (baseUrl.protocol === 'https:' ? '443' : '80'),
    10,
  )
  if (loopback && effectivePort === 9022) {
    throw new Error('port 9022 is protected and cannot be used by validation load runners')
  }
  const allowRemote = explicitBoolean(args.allowRemote ?? env.KIRO_LOAD_ALLOW_REMOTE)
  if (!loopback && !allowRemote) {
    throw new Error('non-loopback load targets require explicit --allow-remote true')
  }

  const apiKey = args.apiKey || env.KIRO_API_KEY
  if (!apiKey || !apiKey.trim()) {
    throw new Error('an explicit --api-key or KIRO_API_KEY is required')
  }
  return { baseUrl, apiKey }
}
