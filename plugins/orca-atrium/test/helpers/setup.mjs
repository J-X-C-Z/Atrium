import { mkdtempSync, realpathSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { Service } from '../.build/service.mjs'
import { FakeCore } from './fake-core.mjs'
import { FakeHost } from './fake-host.mjs'

export function workspace() {
  const dir = realpathSync(mkdtempSync(join(tmpdir(), 'atr-')))
  return dir
}

export function serviceOptions(dir, core, extra = {}) {
  return { core, binDir: join(import.meta.dirname, '..', '..', 'bin'), socket: join(dir, 'a.sock'), cli: 'atrium', userTemplates: join(dir, 'user-templates'), graceMs: 60_000, tickMs: 3_600_000, ...extra }
}

export async function setup({ settings, graceMs } = {}) {
  const dir = workspace()
  const host = new FakeHost({ workspacePath: dir, settings })
  const core = new FakeCore()
  const service = await Service.create(host, serviceOptions(dir, core, graceMs ? { graceMs } : {}))
  return { dir, host, core, service }
}
