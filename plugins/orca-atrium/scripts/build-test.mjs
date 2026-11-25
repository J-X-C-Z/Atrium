// Bundles the worker service for the tests (they run plain JS, no TypeScript loader needed).
import { build } from 'esbuild'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')
await build({
  entryPoints: [join(root, 'worker/src/service.ts')],
  outfile: join(root, 'test/.build/service.mjs'),
  bundle: true,
  platform: 'node',
  format: 'esm',
  target: 'node20',
  logLevel: 'warning'
})
