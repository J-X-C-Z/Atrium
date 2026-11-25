// The agent.launch.before hook: our launches get PATH and ATRIUM_* env, other launches are left alone.
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { delimiter, join } from 'node:path'
import { until } from './helpers/fake-host.mjs'
import { setup } from './helpers/setup.mjs'

test('only launches the worker started get the CLI on PATH and the ATRIUM env', async () => {
  const { host, dir, service } = await setup()
  try {
    // Someone else launching an agent in the same workspace: not ours.
    const foreign = await host.hooks.get('agent.launch.before')(
      { agent: 'claude', workspaceId: 'ws1', workspaceKind: 'folder', command: 'claude', env: { PATH: '/usr/bin' } },
      (e) => ({ next: e })
    )
    assert.equal(foreign, undefined)

    const { dir: runDir } = await host.request('run.start', { templateId: 'demo', inputs: { topic: 'x' } })
    await until(() => host.launches.length === 1)
    const launch = host.launches[0]
    assert.equal(launch.hooked, true)
    assert.equal(launch.env.PATH.split(delimiter)[0], join(import.meta.dirname, '..', 'bin'))
    assert.ok(launch.env.PATH.endsWith('/usr/bin:/bin'))
    assert.equal(launch.env.ATRIUM_SOCKET, join(dir, 'a.sock'))
    assert.equal(launch.env.ATRIUM_RUN, runDir)
    assert.ok(launch.env.ATRIUM_TOKEN.length > 10)
    assert.ok(launch.env.ATRIUM_LAUNCH)

    // The same event again no longer matches (one launch, one hook).
    const again = await host.hooks.get('agent.launch.before')(
      { agent: 'claude', workspaceId: 'ws1', workspaceKind: 'folder', command: 'claude', env: {} },
      (e) => ({ next: e })
    )
    assert.equal(again, undefined)

    // A caller without the token is refused.
    const bad = await host.atrium('t1', ['status'])
    assert.equal(bad.code, 0)
    const noToken = await new Promise((resolve) => {
      import('node:child_process').then(({ execFile }) =>
        execFile(process.execPath, [join(import.meta.dirname, '..', 'bin', 'atrium'), 'status'], { env: { ...process.env, ATRIUM_SOCKET: join(dir, 'a.sock'), ATRIUM_TOKEN: 'nope' } }, (e, out, err) => resolve({ code: e?.code ?? 0, err }))
      )
    })
    assert.equal(noToken.code, 1)
    assert.match(noToken.err, /bad token/)
  } finally {
    await service.dispose()
  }
})

test('help works without a socket and unknown commands fail', async () => {
  const { execFileSync } = await import('node:child_process')
  const bin = join(import.meta.dirname, '..', 'bin', 'atrium')
  assert.match(execFileSync(process.execPath, [bin, 'help'], { env: {} }).toString(), /atrium report/)
  assert.throws(() => execFileSync(process.execPath, [bin, 'frobnicate'], { env: {}, stdio: 'pipe' }))
})
