import { chromium } from 'playwright'
import { writeFile, mkdir } from 'node:fs/promises'
import path from 'node:path'

const baseUrl = process.env.ADMIN_UI_URL || 'http://127.0.0.1:9026'
const apiKey = process.env.ADMIN_API_KEY || 'sk-admin-local-debug'
const outDir = process.env.UI_SMOKE_OUT || '/tmp/kiro-admin-ui-daisy-smoke'

async function waitForApp(page) {
  await page.waitForLoadState('networkidle')
  await page.waitForTimeout(250)
}

async function closeModal(page, name) {
  const dialog = page.getByRole('dialog').filter({ has: page.getByRole('heading', { name }) })
  await dialog.getByRole('button', { name: '关闭' }).first().click()
  await dialog.waitFor({ state: 'hidden' })
}

const pageHeadingByNav = {
  凭据: '凭据控制台',
  使用记录: '使用记录',
  模型价格: '模型价格与能力',
  审计日志: '审计日志',
  运行配置: '运行时配置',
}

async function openNav(page, name) {
  await page.getByRole('button', { name: new RegExp(name) }).first().click()
  await expectVisible(page.locator('h1', { hasText: pageHeadingByNav[name] || name }))
  await waitForApp(page)
}

async function expectVisible(locator, timeout = 12000) {
  await locator.first().waitFor({ state: 'visible', timeout })
}

async function expectText(page, text, timeout = 12000) {
  await expectVisible(page.getByText(text, { exact: false }), timeout)
}

async function fillFileInput(page, labelText, fileName, content) {
  const input = page.locator('label', { hasText: labelText }).locator('input[type=file]').first()
  await input.setInputFiles({
    name: fileName,
    mimeType: 'application/json',
    buffer: Buffer.from(content),
  })
}

async function main() {
  await mkdir(outDir, { recursive: true })
  const browser = await chromium.launch()
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } })
  const logs = []
  const errors = []

  page.on('console', (message) => {
    if (message.type() === 'error') errors.push(`console: ${message.text()}`)
  })
  page.on('pageerror', (error) => errors.push(`pageerror: ${error.message}`))
  page.on('requestfailed', (request) => {
    const failure = request.failure()
    const url = request.url()
    if (!url.includes('/api/admin/')) errors.push(`request failed: ${url} ${failure?.errorText || ''}`)
  })

  await page.goto(baseUrl, { waitUntil: 'networkidle' })
  logs.push('打开登录页')
  await expectVisible(page.getByRole('heading', { name: '登录后台' }))
  await page.getByPlaceholder('sk-admin-...').fill(apiKey)
  await page.getByRole('button', { name: '进入后台' }).click()
  await expectVisible(page.locator('h1', { hasText: '凭据控制台' }))
  await expectText(page, '添加凭据')
  logs.push('登录成功并进入凭据页')

  await page.screenshot({ path: path.join(outDir, '01-credentials.png'), fullPage: true })

  await page.getByRole('button', { name: '添加凭据' }).click()
  await expectVisible(page.getByRole('heading', { name: '添加凭据' }))
  await page.getByLabel('账号邮箱').fill('ui-smoke@example.com')
  await page.getByLabel('认证方式').selectOption('api_key')
  await page.getByLabel('Kiro API Key').fill('ksk_smoke_fake')
  await fillFileInput(page, '从文件填充', 'credential.json', JSON.stringify({ authMethod: 'api_key', kiroApiKey: 'ksk_file_fake', email: 'file-smoke@example.com' }))
  await expectText(page, '已填充第一条凭据')
  await closeModal(page, '添加凭据')
  logs.push('添加凭据弹窗、表单和文件填充通过')

  await page.getByRole('button', { name: '批量导入' }).click()
  await expectVisible(page.getByRole('heading', { name: '批量导入凭据（自动验活）' }))
  await fillFileInput(page, '选择文件', 'credentials.jsonl', '{"authMethod":"api_key","kiroApiKey":"ksk_batch_fake_1","email":"batch1@example.com"}\n{"authMethod":"api_key","kiroApiKey":"ksk_batch_fake_2","email":"batch2@example.com"}')
  await expectText(page, '已从 1 个文件读取 2 条凭据')
  await closeModal(page, '批量导入凭据（自动验活）')
  logs.push('批量导入弹窗和多凭据文件读取通过')

  await page.getByRole('button', { name: 'KAM 导入' }).click()
  await expectVisible(page.getByRole('heading', { name: 'Kiro Account Manager 导入（自动验活）' }))
  await fillFileInput(page, '选择文件', 'kam.json', JSON.stringify({ accounts: [{ email: 'kam@example.com', status: 'ok', credentials: { refreshToken: 'rt_fake' } }] }))
  await expectText(page, '已从 1 个文件读取 1 个账号')
  await expectText(page, '识别到 1 个账号')
  await closeModal(page, 'Kiro Account Manager 导入（自动验活）')
  logs.push('KAM 导入弹窗和文件读取通过')

  await page.getByRole('button', { name: '导出' }).click()
  await expectVisible(page.getByRole('heading', { name: '导出凭据' }))
  await page.getByRole('button', { name: /JSONL/ }).click()
  await closeModal(page, '导出凭据')
  logs.push('导出弹窗和格式选择通过')

  const testButtons = page.getByRole('button', { name: /^测试$/ })
  if (await testButtons.count()) {
    await testButtons.first().click()
    await expectVisible(page.getByRole('heading', { name: '测试模型调用' }))
    await page.getByLabel('测试模型').selectOption({ index: 0 })
    await page.getByLabel('测试消息').fill('ui smoke only')
    await closeModal(page, '测试模型调用')
    logs.push('凭据测试弹窗和模型下拉通过')
  } else {
    logs.push('当前没有凭据卡片，跳过凭据测试弹窗')
  }

  const balanceButtons = page.getByRole('button', { name: /^余额$/ })
  if (await balanceButtons.count()) {
    await balanceButtons.first().click()
    await expectVisible(page.getByRole('heading', { name: /余额信息/ }))
    await closeModal(page, /余额信息/)
    logs.push('余额弹窗通过')
  } else {
    logs.push('当前没有凭据卡片，跳过余额弹窗')
  }

  await openNav(page, '使用记录')
  await page.screenshot({ path: path.join(outDir, '02-usage.png'), fullPage: true })
  await page.getByPlaceholder('搜索模型、账号、会话、错误').fill('claude')
  await page.getByRole('textbox', { name: '模型', exact: true }).fill('claude-sonnet')
  await page.getByPlaceholder('账号 ID').fill('1')
  await page.locator('select').nth(0).selectOption('error')
  await waitForApp(page)
  await page.getByRole('button', { name: '重置' }).click()
  await waitForApp(page)
  const detailButtons = page.getByRole('button', { name: /^详情$/ })
  if (await detailButtons.count()) {
    await detailButtons.first().click()
    await expectVisible(page.getByRole('heading', { name: '使用详情' }))
    await closeModal(page, '使用详情')
    logs.push('使用记录筛选、重置和详情弹窗通过')
  } else {
    logs.push('使用记录筛选和重置通过，当前页没有详情按钮')
  }

  await openNav(page, '模型价格')
  await expectText(page, 'Kiro 模型能力目录')
  await expectText(page, '关注模型价格')
  await page.screenshot({ path: path.join(outDir, '03-pricing.png'), fullPage: true })
  logs.push('模型价格页面加载通过')

  await openNav(page, '审计日志')
  await page.screenshot({ path: path.join(outDir, '04-audit.png'), fullPage: true })
  const auditDetailButtons = page.getByRole('button', { name: '查看审计详情' })
  if (await auditDetailButtons.count()) {
    await auditDetailButtons.first().click()
    await expectVisible(page.getByRole('heading', { name: '审计详情' }))
    await closeModal(page, '审计详情')
    logs.push('审计日志页面和详情弹窗通过')
  } else {
    logs.push('审计日志页面加载通过，当前没有详情行')
  }

  await openNav(page, '运行配置')
  await expectText(page, '凭据限速与冷却')
  await expectText(page, '路径级 Usage 上报改写')
  await page.getByRole('button', { name: '添加路径覆盖' }).click()
  await page.locator('input[value="/new"]').waitFor({ state: 'visible' })
  logs.push('运行配置页面、配置分组和新增路径覆盖交互通过（未保存）')
  await page.screenshot({ path: path.join(outDir, '05-config.png'), fullPage: true })

  const beforeTheme = await page.evaluate(() => document.documentElement.dataset.theme)
  await page.getByRole('button', { name: /主题/ }).click()
  const afterTheme = await page.evaluate(() => document.documentElement.dataset.theme)
  if (beforeTheme === afterTheme) throw new Error(`主题切换失败: ${beforeTheme}`)
  await page.screenshot({ path: path.join(outDir, '06-theme.png'), fullPage: true })
  logs.push(`主题切换通过: ${beforeTheme} -> ${afterTheme}`)

  await page.setViewportSize({ width: 390, height: 844 })
  await page.reload({ waitUntil: 'networkidle' })
  await expectVisible(page.getByRole('heading', { name: '凭据控制台' }))
  await page.getByRole('button', { name: '凭据', exact: true }).click()
  await page.locator('.glass-nav').getByText('运行配置', { exact: true }).click()
  await expectVisible(page.getByRole('heading', { name: '运行时配置' }))
  await page.screenshot({ path: path.join(outDir, '07-mobile.png'), fullPage: true })
  logs.push('移动端顶部导航和页面渲染通过')

  if (errors.length) {
    await writeFile(path.join(outDir, 'errors.log'), errors.join('\n'))
    throw new Error(`浏览器控制台或请求失败: ${errors.join(' | ')}`)
  }

  await writeFile(path.join(outDir, 'result.log'), logs.join('\n'))
  await browser.close()
  console.log(logs.join('\n'))
  console.log(`screenshots=${outDir}`)
}

main().catch(async (error) => {
  console.error(error)
  process.exit(1)
})
