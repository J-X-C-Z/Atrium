// A project autopilot round: a lead starts on a ready task, hits its usage limit, and a stand-in takes over.
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { until } from './helpers/fake-host.mjs'
import { setup } from './helpers/setup.mjs'

test('autopilot launches a role, a quota hit hands it to a stand-in, which finishes the task', async () => {
  const { host, dir, core, service } = await setup()
  try {
    core.addProject(dir)
    const overview = await host.request('overview.get')
    assert.equal(overview.projects.length, 1)
    assert.equal(overview.projects[0].autopilot, false)

    await host.request('project.autopilot', { root: dir, on: true })
    await until(() => host.launches.length === 1, 4000, 'lead launch')
    const lead = host.launches[0]
    assert.equal(lead.agent, 'claude')
    assert.match(lead.prompt, /Kickoff for lead:api on t1/)
    assert.match(lead.prompt, /atrium task done t1/)
    assert.equal(lead.env.ATRIUM_PROJECT, dir)
    assert.equal(lead.env.ATRIUM_ROLE, 'lead:api')

    await host.status_('t1', 'working')
    let view = await host.request('project.get', { root: dir })
    const node = view.org.children[0]
    assert.equal(node.live, 'working')
    assert.equal(node.terminalId, 't1')

    // The agent runs into its limit.
    host.terminalText.set('t1', ['working…', "You've hit your usage limit. Limit resets at 5pm."])
    await host.status_('t1', 'waiting')
    await until(() => host.launches.length === 2, 4000, 'stand-in launch')
    const standIn = host.launches[1]
    assert.equal(standIn.agent, 'codex')
    assert.match(standIn.prompt, /alice hit a usage limit; you \(bob\) are standing in/)
    assert.deepEqual(host.closed, ['t1'])
    assert.equal(service.store.data.quotas.alice.limited, true)
    assert.ok(service.needs.list().some((n) => n.kind === 'quota' && /bob stands in/.test(n.title)))

    view = await host.request('project.get', { root: dir })
    assert.equal(view.org.children[0].member, 'bob')
    assert.equal(view.org.children[0].standingInFor, 'alice')
    assert.equal(view.org.children[0].terminalId, 't2')

    // The stand-in does not get launched twice.
    await service.projects.tick()
    await service.settle()
    assert.equal(host.launches.length, 2)

    // It reports the task through the CLI.
    const done = await host.atrium('t2', ['task', 'done', 't1', 'API is built'])
    assert.equal(done.code, 0, done.err)
    assert.equal(core.projects.get(dir).tasks[0].status, 'done')
    assert.match(core.projects.get(dir).notices.at(-1).text, /^\[lead:api\] t1 done: API is built/)
    assert.equal(core.projects.get(dir).bound['lead:api'], 't2')

    const notice = await host.atrium('t2', ['notice', 'contract changed'])
    assert.equal(notice.code, 0, notice.err)
    assert.equal(core.projects.get(dir).notices.at(-1).text, '[lead:api] contract changed')
    const status = await host.atrium('t2', ['status'])
    assert.match(status.out, /demo · lead:api/)
  } finally {
    await service.dispose()
  }
})

test('atrium ask raises a question; answering types the reply into the terminal', async () => {
  const { host, dir, core, service } = await setup()
  try {
    core.addProject(dir)
    await host.request('project.autopilot', { root: dir, on: true })
    await until(() => host.launches.length === 1)
    const asked = await host.atrium('t1', ['ask', 'Postgres or SQLite?'])
    assert.equal(asked.code, 0, asked.err)
    const need = (await host.request('overview.get')).needs.find((n) => n.kind === 'question')
    assert.equal(need.detail, 'Postgres or SQLite?')
    assert.equal(need.terminalId, 't1')
    await host.request('need.resolve', { id: need.id, answer: 'SQLite' })
    assert.match(host.sent.at(-1).text, /SQLite/)
    assert.equal((await host.request('overview.get')).needs.length, 0)
  } finally {
    await service.dispose()
  }
})

test('opening a board writes the link file and opens it in the workspace', async () => {
  const { host, dir, core, service } = await setup()
  try {
    core.addProject(dir)
    await host.request('board.open', { root: dir })
    assert.deepEqual(host.opened.at(-1), { workspaceId: 'ws1', path: '.atrium/atrium.board' })
    const { readFileSync } = await import('node:fs')
    const text = readFileSync(`${dir}/.atrium/atrium.board`, 'utf8')
    assert.deepEqual(await host.request('link.read', { text }), { kind: 'board', root: dir })
    assert.match(readFileSync(`${dir}/.atrium/.gitignore`, 'utf8'), /atrium\.board/)
  } finally {
    await service.dispose()
  }
})
