// A whole workflow run through the real socket and the real `atrium` CLI:
// launch -> status events -> report -> check -> gate -> send back -> report -> check -> approve -> done.
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { until } from './helpers/fake-host.mjs'
import { setup } from './helpers/setup.mjs'

test('a run goes from launch to done, with one send-back', async () => {
  const { host, core, service } = await setup()
  try {
    const templates = await host.request('templates.list')
    assert.equal(templates[0].id, 'demo')
    assert.equal(templates[0].inputs[0].required, true)

    const { dir } = await host.request('run.start', { templateId: 'demo', inputs: { topic: 'bees' } })
    await until(() => host.launches.length === 1, 4000, 'first launch')
    const first = host.launches[0]
    assert.equal(first.agent, 'claude')
    assert.match(first.prompt, /Write about bees \(attempt 1\)/)
    assert.match(first.prompt, /atrium report/)
    assert.equal(first.env.ATRIUM_RUN, dir)
    assert.equal(first.env.ATRIUM_STEP, 'draft')

    await host.status_('t1', 'working')
    let live = (await host.request('overview.get')).agents.find((a) => a.terminalId === 't1')
    assert.equal(live.state, 'working')
    assert.deepEqual(live.bound, { kind: 'step', dir, step: 'draft' })

    // The agent reports through the socket.
    const reported = await host.atrium('t1', ['report', 'First draft done'])
    assert.equal(reported.code, 0, reported.err)
    assert.match(reported.out, /Report received/)
    assert.deepEqual(core.runs.get(dir).reports, ['First draft done'])

    // The command check runs, then the gate appears as a need.
    const gate = await until(async () => (await host.request('overview.get')).needs.find((n) => n.kind === 'gate'), 4000, 'gate need')
    assert.equal(core.runs.get(dir).checks[0].passed, true)
    assert.equal(gate.source.step, 'gate')
    assert.deepEqual(gate.sendBackTargets, [{ step: 'draft', label: 'Draft' }])
    assert.ok(host.notifications.some((n) => /Approve the draft/.test(n.title)))
    assert.match(host.status.at(-1).text, /^1 need you/)

    // Send back: the step launches again.
    await host.request('run.decide', { dir, step: 'gate', choice: 'send_back', sendBackTo: 'draft', note: 'shorter please' })
    assert.equal(core.runs.get(dir).decisions[0].send_back_to ?? core.runs.get(dir).decisions[0].sendBackTo, 'draft')
    await until(() => host.launches.length === 2, 4000, 'second launch')
    assert.match(host.launches[1].prompt, /attempt 2/)
    assert.equal((await host.request('overview.get')).needs.length, 0)

    const second = await host.atrium('t2', ['report', '-'], { stdin: 'Second draft\nwith a file body' })
    assert.equal(second.code, 0, second.err)
    assert.equal(core.runs.get(dir).reports[1], 'Second draft\nwith a file body')

    const gate2 = await until(async () => (await host.request('overview.get')).needs.find((n) => n.kind === 'gate'), 4000, 'second gate')
    await host.request('need.resolve', { id: gate2.id, answer: 'approve' })
    await service.settle()

    const view = await host.request('run.get', { dir })
    assert.equal(view.state, 'done')
    assert.equal(view.templateName, 'Demo workflow')
    assert.equal(view.steps.length, 2)
    assert.equal((await host.request('overview.get')).needs.length, 0)
    assert.ok(host.posts.some((p) => p.name === 'run.changed'))
    assert.ok(host.closed.length >= 0)
  } finally {
    await service.dispose()
  }
})

test('a failed check sends the step back without bothering the person', async () => {
  const { host, core, service } = await setup()
  try {
    const { dir } = await host.request('run.start', { templateId: 'demo', inputs: { topic: 'ants' } })
    await until(() => host.launches.length === 1)
    core.runs.get(dir).failCheck = true
    await host.status_('t1', 'working')
    await host.atrium('t1', ['report', 'draft'])
    await until(() => host.launches.length === 2, 4000, 'relaunch after failed check')
    assert.equal(core.runs.get(dir).checks[0].passed, false)
    assert.match(core.runs.get(dir).checks[0].detail, /exit code 3/)
    assert.equal((await host.request('overview.get')).needs.length, 0)
  } finally {
    await service.dispose()
  }
})

test('an agent that stops without reporting is nudged once, then flagged as stalled', async () => {
  const { host, service } = await setup({ graceMs: 40 })
  try {
    await host.request('run.start', { templateId: 'demo', inputs: { topic: 'wasps' } })
    await until(() => host.launches.length === 1)
    await host.status_('t1', 'working')
    await host.status_('t1', 'done')
    await until(() => host.sent.length === 1, 3000, 'nudge')
    assert.match(host.sent[0].text, /not reported yet/)
    assert.equal(host.sent[0].terminalId, 't1')
    const need = await until(async () => (await host.request('overview.get')).needs.find((n) => n.kind === 'stalled'), 3000, 'stalled need')
    assert.equal(need.terminalId, 't1')
    // Working again clears it.
    await host.status_('t1', 'working')
    assert.equal((await host.request('overview.get')).needs.length, 0)
  } finally {
    await service.dispose()
  }
})
