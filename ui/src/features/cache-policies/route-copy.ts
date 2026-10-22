import { canonicalCachePolicyPath } from '@/features/runtime/runtime-sections'

/**
 * Client base path for a route, as configured in ANTHROPIC_BASE_URL.
 *
 * `/v1` is the default Anthropic route, so its base is the server root: clients append
 * `/v1/messages` themselves. Every other route is mounted under its own prefix
 * (`/cc/v1`, `/dfcache/{name}/v1`, ...), so the prefix is the base path.
 */
export function routeBasePath(prefix: string): string {
  const canonical = canonicalCachePolicyPath(prefix)
  return canonical === '/v1' || canonical === '/' ? '' : canonical
}

/** Origin the console is served from; the API shares it, so it is the default proxy address. */
export function currentOrigin(): string {
  return typeof window === 'undefined' ? '' : window.location.origin
}

/** Full client base URL, e.g. `https://proxy.example.com/dfcache/team-a`. */
export function routeBaseUrl(prefix: string, origin = currentOrigin()): string {
  return `${origin.replace(/\/+$/, '')}${routeBasePath(prefix)}`
}

/** Claude Code environment snippet for the route; the key is left for the user to fill in. */
export function routeClaudeCodeEnv(prefix: string, origin = currentOrigin()): string {
  return `ANTHROPIC_BASE_URL=${routeBaseUrl(prefix, origin)}\nANTHROPIC_API_KEY=<请求 Key>`
}
