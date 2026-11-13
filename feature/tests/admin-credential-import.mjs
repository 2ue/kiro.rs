// Seeds credentials into a running kiro-rs through the Admin batch import API. The service no
// longer reads a credentials file at startup.
//
// `warmupRemaining` defaults to 0: the removed file bootstrap wrote no runtime state, so seeded
// credentials were schedulable at full weight right away. Pass a number to override it.
export async function importCredentialsViaAdmin(baseUrl, adminKey, credentials, options = {}) {
  const warmupRemaining = options.warmupRemaining ?? 0
  const response = await fetch(`${baseUrl}/api/admin/credentials/import`, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      'x-api-key': adminKey,
      connection: 'close',
    },
    body: JSON.stringify({
      duplicateMode: 'error',
      continueOnError: false,
      autoDiscoverSupportedModels: false,
      credentials: credentials.map((credential) => {
        const { id: _ignoredId, ...fields } = credential
        return { warmupRemaining, ...fields }
      }),
    }),
    signal: AbortSignal.timeout(options.timeoutMs ?? 60_000),
  })
  const text = await response.text()
  if (response.status !== 200) {
    throw new Error(`credential import failed: HTTP ${response.status}`)
  }
  const report = JSON.parse(text)
  if (report.success !== credentials.length || report.failed !== 0) {
    const errors = (report.items || [])
      .filter((item) => !item.ok)
      .map((item) => `#${item.index}: ${item.error}`)
    throw new Error(
      `credential import created ${report.success}/${credentials.length}, failed=${report.failed} ${errors.join('; ')}`,
    )
  }
  // Batch import is sequential, so ids follow the input order.
  return report.items.map((item) => item.credentialId)
}
