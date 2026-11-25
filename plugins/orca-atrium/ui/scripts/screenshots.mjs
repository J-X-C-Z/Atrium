// Screenshots of every view through the mock host. `node scripts/screenshots.mjs [--final] [view…]`
import { createRequire } from 'node:module'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { dirname, join } from 'node:path'
import { mkdirSync } from 'node:fs'
const require = createRequire(import.meta.url)
const { chromium } = require('playwright')
const here = dirname(fileURLToPath(import.meta.url))
const ui = join(here, '..')
const args = process.argv.slice(2)
const final = args.includes('--final')
const only = args.filter((a) => !a.startsWith('--'))
const out = final ? join(ui, '..', 'docs', 'screenshots') : join(process.env.SHOT_DIR ?? '/tmp/atrium-shots')
mkdirSync(out, { recursive: true })

// name, view, size, extra query, action run before the shot
const shots = [
  ['sidebar', 'sidebar', [380, 900], ''],
  ['sidebar-empty', 'sidebar', [380, 640], '&scenario=empty'],
  ['board', 'board', [1440, 900], ''],
  ['board-tasks', 'board', [1440, 900], '', async (p) => { await p.getByRole('tab', { name: /任务|Tasks/ }).click() }],
  ['board-team', 'board', [1440, 900], '', async (p) => { await p.getByRole('tab', { name: /团队|Team/ }).click() }],
  ['run', 'run', [1440, 900], ''],
  ['template', 'template', [1440, 900], ''],
]

const browser = await chromium.launch()
for (const theme of ['light', 'dark']) {
  for (const [name, view, [w, h], extra, action] of shots) {
    if (only.length && !only.some((o) => name.startsWith(o))) continue
    const ctx = await browser.newContext({ viewport: { width: w, height: h }, deviceScaleFactor: final ? 2 : 1, reducedMotion: 'reduce' })
    const page = await ctx.newPage()
    page.on('pageerror', (e) => console.log('pageerror', name, e.message))
    page.on('console', (m) => m.type() === 'error' && console.log('console', name, m.text()))
    await page.goto(pathToFileURL(join(ui, 'dev.html')).href + `?view=${view}&theme=${theme}&lang=${process.env.LANG_Q ?? 'zh'}${extra}`)
    await page.waitForTimeout(700)
    if (action) {
      await action(page)
      await page.waitForTimeout(400)
    }
    await page.screenshot({ path: join(out, `${name}-${theme}.png`) })
    await ctx.close()
  }
}
await browser.close()
console.log('saved to', out)
