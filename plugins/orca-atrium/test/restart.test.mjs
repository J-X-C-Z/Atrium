// A worker restart picks up where it left off: bindings, needs and run state survive.
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { Service } from './.build/service.mjs'
import { until } from './helpers/fake-host.mjs'
import { serviceOptions, setup } from './helpers/setup.mjs'

test('after a restart the same agent can still report and the run goes on', async () => {
  const { host, dir, core, service } = await setup()
  const { dir: runDir } = await host.request('run.start', { templateId: 'demo', inputs: { topic: 'owls' } })
  await until(() => host.launches.length === 1)
  await host.status_('t1', 'working')
  await service.dispose()

  // New worker, same storage and same run files; Orca still lists the agent as working.
  host.handlers.clear()
  host.agentsListed = [{ terminalId: 't1', title: 'claude', agent: 'claude', state: 'working', stateStartedAt: 1 }]
  const again = await Service.create(host, serviceOptions(dir, core))
  try {
    assert.equal(again.runs.metas().length, 1)
    assert.equal(again.store.live().length, 1)
    assert.equal(again.agents.list()[0].state, 'working')

    const reported = await host.atrium('t1', ['report', 'done after restart'])
    assert.equal(reported.code, 0, reported.err)
    const gate = await until(async () => (await host.request('overview.get')).needs.find((n) => n.kind === 'gate'), 4000, 'gate after restart')
    assert.equal(gate.source.dir, runDir)
    // No second agent was launched for the step that already had one.
    assert.equal(host.launches.length, 1)
    assert.equal(host.launches.at(-1).terminalId, 't1')
  } finally {
    await again.dispose()
  }
})

test('a terminal that vanished while the worker was down restarts its step', async () => {
  const { host, dir, core, service } = await setup()
  await host.request('run.start', { templateId: 'demo', inputs: { topic: 'crows' } })
  await until(() => host.launches.length === 1)
  await service.dispose()

  host.handlers.clear()
  host.agentsListed = []
  const again = await Service.create(host, serviceOptions(dir, core))
  try {
    await until(() => host.launches.length === 2, 4000, 'step relaunched')
    assert.equal(host.launches[1].terminalId, 't2')
    assert.equal(again.store.live().length, 1)
  } finally {
    await again.dispose()
  }
})
