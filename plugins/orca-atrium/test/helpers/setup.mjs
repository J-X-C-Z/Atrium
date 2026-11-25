import { createHash } from 'node:crypto'
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

export function testSocket(dir) {
  const tag = createHash('sha256').update(dir).digest('hex').slice(0, 12)
  return process.platform === 'win32' ? `\\\\.\\pipe\\atrium-test-${tag}` : join(dir, 'a.sock')
}

export function serviceOptions(dir, core, extra = {}) {
  return { core, binDir: join(import.meta.dirname, '..', '..', 'bin'), socket: testSocket(dir), cli: 'atrium', userTemplates: join(dir, 'user-templates'), graceMs: 60_000, tickMs: 3_600_000, ...extra }
}

export async function setup({ settings, graceMs } = {}) {
  const dir = workspace()
  const host = new FakeHost({ workspacePath: dir, settings })
  const core = new FakeCore()
  const service = await Service.create(host, serviceOptions(dir, core, graceMs ? { graceMs } : {}))
  return { dir, host, core, service }
}
