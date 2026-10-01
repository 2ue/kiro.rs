// 业务代码规范检查：shadcn 组件之外不得直接使用原生交互元素、色板类和任意字号，单文件不超过 400 行。
// 允许在行尾用 `// ui-rules-allow: <原因>` 豁免单行（例如需要原生 <input type="file">）。
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join, relative } from 'node:path'

const ROOT = new URL('../src', import.meta.url).pathname
const SKIP = ['components/ui/', 'routeTree.gen.ts', '__tests__/', 'shadcn-tailwind.css']
const MAX_LINES = 400

const RULES = [
  { id: 'native-element', re: /<(button|input|select|textarea)\b/, msg: '使用 @/components/ui 中的组件代替原生元素' },
  {
    id: 'palette-class',
    re: /\b(?:text|bg|border|ring|fill|stroke|from|to|via)-(?:red|green|blue|yellow|amber|emerald|rose|cyan|purple|violet|indigo|orange|lime|teal|sky|pink|fuchsia|slate|gray|zinc|stone|neutral)-\d{2,3}\b/,
    msg: '使用语义 token（success/warning/danger/info/primary…）代替色板类',
  },
  { id: 'arbitrary-font', re: /\btext-\[\d+(?:\.\d+)?(?:px|rem)\]/, msg: '使用字号阶梯（text-xs/sm/base…）代替任意字号' },
]

function walk(dir) {
  return readdirSync(dir).flatMap((name) => {
    const p = join(dir, name)
    return statSync(p).isDirectory() ? walk(p) : /\.(tsx?|css)$/.test(name) ? [p] : []
  })
}

const problems = []
for (const file of walk(ROOT)) {
  const rel = relative(ROOT, file)
  if (SKIP.some((s) => rel.includes(s))) continue
  const lines = readFileSync(file, 'utf8').split('\n')
  if (lines.length > MAX_LINES) problems.push(`${rel}: ${lines.length} 行，超过 ${MAX_LINES} 行上限，请拆分`)
  lines.forEach((line, i) => {
    if (line.includes('ui-rules-allow:')) return
    for (const r of RULES) if (r.re.test(line)) problems.push(`${rel}:${i + 1} [${r.id}] ${r.msg}`)
  })
}

if (problems.length) {
  console.error(problems.join('\n'))
  console.error(`\n${problems.length} 处违反 UI 规范`)
  process.exit(1)
}
console.log('UI 规范检查通过')
