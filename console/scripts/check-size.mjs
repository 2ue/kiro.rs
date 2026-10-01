// 首屏体积检查：入口 JS + modulepreload 的 chunk（gzip）不超过预算。需先执行 pnpm build。
import { readFileSync } from 'node:fs'
import { gzipSync } from 'node:zlib'

const BUDGET_KB = 200
const dist = new URL('../dist/', import.meta.url)
const html = readFileSync(new URL('index.html', dist), 'utf8')
const files = [
  ...html.matchAll(/<script type="module" crossorigin src="\/console\/assets\/([^"]+)"/g),
  ...html.matchAll(/<link rel="modulepreload" crossorigin href="\/console\/assets\/([^"]+)"/g),
].map((m) => m[1])
const total = files.reduce((sum, f) => sum + gzipSync(readFileSync(new URL(`assets/${f}`, dist))).length, 0) / 1024
console.log(`首屏 JS（gzip）：${total.toFixed(1)} KB / 预算 ${BUDGET_KB} KB，共 ${files.length} 个文件`)
if (total > BUDGET_KB) process.exit(1)
