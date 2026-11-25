// Bundles each view into one self-contained HTML file (frames have no network).
// `node build.mjs` → dist/{sidebar,board,run,template}.html
// `node build.mjs --dev` also writes dev-dist/dev.{js,css} for dev.html (mock host).
// `--watch` rebuilds the dev bundle on change.
import * as esbuild from 'esbuild'
import { mkdir, writeFile, rm } from 'node:fs/promises'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const args = new Set(process.argv.slice(2))
const views = ['sidebar', 'board', 'run', 'template']
const titles = { sidebar: 'Atrium', board: 'Atrium board', run: 'Atrium run', template: 'Atrium template' }

const common = {
  bundle: true,
  format: 'iife',
  target: ['chrome120'],
  jsx: 'automatic',
  jsxImportSource: 'preact',
  legalComments: 'none',
  logLevel: 'warning',
  loader: { '.svg': 'text' },
}

function inline(view, js, css) {
  // Keep `</script>` and `</style>` sequences from closing the tags early.
  const safeJs = js.replace(/<\/script/gi, '<\\/script').replace(/<!--/g, '<\\!--')
  const safeCss = css.replace(/<\/style/gi, '<\\/style')
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="x-dns-prefetch-control" content="off">
<title>${titles[view]}</title>
<style>${safeCss}</style>
</head>
<body>
<div id="root" data-view="${view}"></div>
<script>${safeJs}</script>
</body>
</html>
`
}

async function buildView(view) {
  const result = await esbuild.build({
    ...common,
    entryPoints: [join(here, 'src/entries', `${view}.tsx`)],
    outdir: join(here, '.out', view),
    write: false,
    minify: true,
    define: { 'process.env.NODE_ENV': '"production"' },
  })
  let js = ''
  let css = ''
  for (const file of result.outputFiles) {
    if (file.path.endsWith('.js')) js = file.text
    else if (file.path.endsWith('.css')) css = file.text
  }
  const html = inline(view, js, css)
  await writeFile(join(here, 'dist', `${view}.html`), html)
  return html.length
}

const devOptions = {
  ...common,
  entryPoints: [join(here, 'src/mock/dev.tsx')],
  outdir: join(here, 'dev-dist'),
  entryNames: 'dev',
  sourcemap: 'inline',
  define: { 'process.env.NODE_ENV': '"development"' },
}

await mkdir(join(here, 'dist'), { recursive: true })
if (args.has('--watch')) {
  const ctx = await esbuild.context(devOptions)
  await ctx.watch()
  console.log('watching dev bundle…')
} else {
  for (const view of views) {
    const size = await buildView(view)
    console.log(`dist/${view}.html  ${(size / 1024).toFixed(1)} KiB`)
  }
  if (args.has('--dev')) {
    await rm(join(here, 'dev-dist'), { recursive: true, force: true })
    await esbuild.build(devOptions)
    console.log('dev-dist/dev.js')
  }
}
