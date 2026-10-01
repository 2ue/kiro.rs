/**
 * 冒烟 E2E：用本机 Chrome + vite preview + 模拟 API 跑关键流程。
 * 不下载浏览器、不依赖后端与数据库；失败截图写入系统临时目录，结束后删除。
 */
import { spawn } from 'node:child_process'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { chromium } from 'playwright-core'
import { createState, route } from './fixtures.mjs'

const PORT = 9037
const BASE = `http://127.0.0.1:${PORT}/console`
const CHROME = process.env.CHROME_PATH ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
const artifacts = mkdtempSync(join(tmpdir(), 'kiro-console-e2e-'))

const preview = spawn('npx', ['vite', 'preview', '--port', String(PORT), '--strictPort'], { stdio: 'ignore' })
const results = []
let browser

async function step(name, fn) {
  try {
    await fn()
    results.push(`✓ ${name}`)
  } catch (e) {
    results.push(`✗ ${name}: ${e.message.split('\n')[0]}`)
    throw e
  }
}

try {
  for (let i = 0; i < 50; i++) {
    try {
      if ((await fetch(BASE + '/')).ok) break
    } catch {}
    await new Promise((r) => setTimeout(r, 200))
  }
  browser = await chromium.launch({ executablePath: CHROME, headless: true })
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } })
  const state = createState()
  const errors = []
  page.on('pageerror', (e) => errors.push(e.message))
  await page.route('**/api/admin/**', async (r) => {
    const req = r.request()
    const url = new URL(req.url())
    const res = route(state, req.method(), url.pathname + url.search, req.postData() ? JSON.parse(req.postData()) : undefined)
    await r.fulfill({ status: res.status, contentType: 'application/json', body: JSON.stringify(res.body) })
  })

  await step('登录', async () => {
    await page.goto(BASE + '/overview')
    await page.getByLabel('Admin API Key').fill('e2e-key')
    await page.getByRole('button', { name: '进入控制台' }).click()
    await page.getByRole('heading', { name: '总览' }).waitFor()
  })

  await step('账号列表与禁用原因', async () => {
    await page.goto(BASE + '/accounts')
    await page.getByText('alice@example.com').waitFor()
    await page.getByText('Refresh Token 失效').first().waitFor()
  })

  await step('禁用账号可撤销（乐观更新）', async () => {
    await page.goto(BASE + '/accounts?id=1')
    await page.getByRole('dialog').getByRole('button', { name: '禁用' }).click()
    await page.getByRole('button', { name: '撤销' }).waitFor()
    if (!state.credentials[0].disabled) throw new Error('禁用请求未发送')
    await page.getByRole('button', { name: '撤销' }).click()
    await page.getByText('已撤销').waitFor()
    if (state.credentials[0].disabled) throw new Error('撤销未生效')
  })

  await step('错误翻译', async () => {
    await page.goto(BASE + '/accounts?id=3')
    await page.getByRole('dialog').getByText('上游限流').waitFor()
  })

  await step('配置保存与三方合并', async () => {
    await page.goto(BASE + '/settings/retry')
    const input = page.getByRole('textbox', { name: '流式保活间隔' })
    await input.fill('8')
    await input.blur()
    await page.getByText('1 项修改未保存').waitFor()
    // 模拟其他页面在此期间修改了外部池字段
    state.runtimeConfig = { ...state.runtimeConfig, externalPools: { ...state.runtimeConfig.externalPools, externalPoolRetryMaxAttempts: 7 } }
    await page.getByRole('button', { name: '保存', exact: true }).click()
    await page.getByText('配置已保存').waitFor()
    const saved = state.puts.at(-1)
    if (saved.streamKeepaliveIntervalSecs !== 8) throw new Error('本地修改未保存')
    if (saved.externalPools.externalPoolRetryMaxAttempts !== 7) throw new Error('远端修改被覆盖')
  })

  await step('命令面板搜索配置项', async () => {
    await page.keyboard.press(process.platform === 'darwin' ? 'Meta+k' : 'Control+k')
    await page.getByPlaceholder(/搜索页面/).fill('保活')
    await page.getByRole('option', { name: /流式保活间隔/ }).click()
    await page.waitForURL(/settings\/retry\?focus=/)
  })

  await step('窄屏卡片列表', async () => {
    await page.setViewportSize({ width: 390, height: 844 })
    await page.goto(BASE + '/accounts')
    await page.getByText('alice@example.com').waitFor()
    if (await page.locator('[role=table]').count()) throw new Error('窄屏仍渲染表格')
  })

  if (errors.length) throw new Error(`页面运行时错误：${errors[0]}`)
} catch (e) {
  try {
    const pages = browser?.contexts()[0]?.pages() ?? []
    if (pages[0]) await pages[0].screenshot({ path: join(artifacts, 'failure.png') })
    console.error(`失败截图：${join(artifacts, 'failure.png')}（查看后执行 pnpm e2e:clean 删除）`)
  } catch {}
  process.exitCode = 1
} finally {
  console.log(results.join('\n'))
  await browser?.close()
  preview.kill()
  if (!process.exitCode) rmSync(artifacts, { recursive: true, force: true })
}
