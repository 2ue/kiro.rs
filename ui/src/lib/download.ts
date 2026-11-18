/** 触发浏览器下载一个 Blob。 */
export function downloadBlob(blob: Blob, filename: string) {
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url; a.download = filename
  document.body.appendChild(a); a.click(); a.remove()
  URL.revokeObjectURL(url)
}

/** `prefix-<ISO 时间>.ext`，文件名里不含冒号和点。 */
export function timestampedFilename(prefix: string, extension: string): string {
  const stamp = new Date().toISOString().replace(/[:.]/g, '-')
  return `${prefix}-${stamp}.${extension}`
}
