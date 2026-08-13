export function formatUsd(value: number | undefined | null): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '-'
  const number = value as number
  const fractionDigits = Math.abs(number) >= 1 ? 2 : 6
  return new Intl.NumberFormat('en-US', {
    style: 'currency',
    currency: 'USD',
    minimumFractionDigits: fractionDigits,
    maximumFractionDigits: fractionDigits,
  }).format(number)
}

export function formatUsdDetailed(value: number | undefined | null): string {
  if (!Number.isFinite(value ?? Number.NaN)) return '-'
  return new Intl.NumberFormat('en-US', {
    style: 'currency',
    currency: 'USD',
    minimumFractionDigits: 8,
    maximumFractionDigits: 8,
  }).format(value as number)
}

export function formatUsdCsv(value: number | undefined | null): string {
  if (!Number.isFinite(value ?? Number.NaN)) return ''
  return (value as number).toFixed(8)
}
