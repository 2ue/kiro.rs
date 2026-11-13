import type { AddCredentialRequest } from '../types/api'

type AuthMethod = NonNullable<AddCredentialRequest['authMethod']>
type AuthFields = Pick<AddCredentialRequest, 'provider' | 'clientId' | 'clientSecret' | 'tokenEndpoint' | 'issuerUrl' | 'scopes' | 'kiroApiKey'> & {
  authMethod?: string | null
}

function compact(value: string | null | undefined): string {
  return value?.toLowerCase().replace(/[^a-z0-9]/g, '') ?? ''
}

function declaredAuthMethod(value: string | null | undefined): AuthMethod | undefined {
  switch (compact(value)) {
    case 'social': return 'social'
    case 'idc': case 'builderid': case 'iam': return 'idc'
    case 'apikey': return 'api_key'
    case 'externalidp': case 'enterprise': case 'iamsso': case 'awsidc': case 'internal': return 'external_idp'
    default: return undefined
  }
}

/** Keep aligned with KiroCredentials::effective_auth_kind, including legacy exports. */
export function resolveCredentialAuthMethod(input: AuthFields): AuthMethod {
  const declared = declaredAuthMethod(input.authMethod)
  if (input.kiroApiKey != null || declared === 'api_key') return 'api_key'
  const hasClients = Boolean(input.clientId?.trim() && input.clientSecret?.trim())
  const hasExternalFields = Boolean(input.tokenEndpoint?.trim() || input.issuerUrl?.trim() || input.scopes?.trim())
  const provider = compact(input.provider)
  const awsProvider = ['builderid', 'enterprise', 'iamsso', 'awsidc', 'internal'].includes(provider)
  if (hasClients && awsProvider && !hasExternalFields && declared === 'social') return 'idc'
  if (declared) return declared
  if (hasExternalFields || (!(hasClients && awsProvider) && ['externalidp', 'enterprise', 'iamsso', 'awsidc', 'internal'].includes(provider))) return 'external_idp'
  return hasClients ? 'idc' : 'social'
}
