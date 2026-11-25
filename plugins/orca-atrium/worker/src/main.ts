// Entry point: Orca calls the default export once when the worker starts.

import { existsSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Core } from './core.js'
import { orcaHost, type OrcaWorker } from './host.js'
import { socketPath } from './bridge/server.js'
import { Service } from './service.js'

let service: Service | null = null

/** Folders the core may read and write (projects and the Atrium config can be anywhere). */
function fsRoots(): string[] {
  if (process.platform !== 'win32') return ['/']
  const drives: string[] = []
  for (let c = 65; c <= 90; c++) {
    const root = `${String.fromCharCode(c)}:\\`
    if (existsSync(root)) drives.push(root)
  }
  return drives
}

export default async function activate(orca: OrcaWorker): Promise<void> {
  // worker/dist/main.mjs -> plugin root
  const root = dirname(dirname(dirname(fileURLToPath(import.meta.url))))
  const host = orcaHost(orca)
  const core = await Core.load(join(root, 'core', 'atrium_core.wasm'), fsRoots())
  const win = process.platform === 'win32'
  const bin = join(root, 'bin')
  service = await Service.create(host, {
    core,
    binDir: bin,
    socket: socketPath(root),
    cli: win ? `"${join(bin, 'atrium.cmd')}"` : `node "${join(bin, 'atrium')}"`
  })
  host.log('atrium: ready')
}

export async function deactivate(): Promise<void> {
  const s = service
  service = null
  await s?.dispose()
}
