export function credentialIdentityLabel(id: number, label?: string): string {
  const trimmed = label?.trim()
  if (!trimmed) return `#${id}`
  const name = trimmed.replace(new RegExp(`^#${id}\\s*`), '')
  return name ? `#${id} ${name}` : `#${id}`
}
