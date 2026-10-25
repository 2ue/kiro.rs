/** Characters of an email's name kept before giving up on showing its domain whole. */
const MIN_HEAD = 4

/**
 * Shortens `text` by replacing its middle with `marker` until `fits` accepts it. An email keeps its
 * domain whole while a few characters of the name still fit, so
 * `virendrayadav9170643364@gmail.com` becomes `virendrayad***@gmail.com`; tighter than that the
 * cut is centred. Text that already ends with the marker (a hidden email suffix) is cut at the end.
 */
export function middleTruncate(text: string, fits: (candidate: string) => boolean, marker = '***'): string {
  if (fits(text)) return text
  if (text.endsWith(marker)) {
    const base = Array.from(text.slice(0, -marker.length))
    for (let keep = base.length - 1; keep > 0; keep -= 1) {
      const candidate = base.slice(0, keep).join('') + marker
      if (fits(candidate)) return candidate
    }
    return marker
  }
  const chars = Array.from(text)
  const at = chars.lastIndexOf('@')
  const domain = at > 0 ? chars.length - at : 0
  for (let keep = chars.length - 1; keep > 0; keep -= 1) {
    const tail = domain > 0 && keep - domain >= MIN_HEAD ? domain : Math.floor(keep / 2)
    const candidate = chars.slice(0, keep - tail).join('') + marker + chars.slice(chars.length - tail).join('')
    if (fits(candidate)) return candidate
  }
  return marker
}
