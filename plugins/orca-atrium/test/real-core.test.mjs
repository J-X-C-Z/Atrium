// The worker on the real wasm core: a one-step run, and a project read, edited and listed.
// Build the core first: sh scripts/build-core.sh
import assert from 'node:assert/strict'
import { existsSync, mkdirSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { test } from 'node:test'
import { Core, ROOT } from '../worker/src/core.ts'
import { Service } from './.build/service.mjs'
import { FakeHost, until } from './helpers/fake-host.mjs'
import { serviceOptions, workspace } from './helpers/setup.mjs'

const wasm = join(import.meta.dirname, '..', 'core', 'atrium_core.wasm')
const TEMPLATE = `
id = "note"
name = "Note"

[[inputs]]
key = "topic"
label = "Topic"
kind = "text"
required = true

[[steps]]
id = "write"
title = "Write"
prompt = "Write a note on {{input.topic}}."
`

test('a run on the real core: template list, start, launch, report, done', { skip: !existsSync(wasm) && 'core not built' }, async () => {
  const dir = workspace()
  const host = new FakeHost({ workspacePath: dir })
  const core = await Core.load(wasm, ROOT)
  const templates = join(dir, 'user-templates')
  mkdirSync(templates, { recursive: true })
  writeFileSync(join(templates, 'note.toml'), TEMPLATE)
  const service = await Service.create(host, serviceOptions(dir, core))
  try {
    const list = await host.request('templates.list')
    const note = list.find((t) => t.id === 'note')
    assert.ok(note, JSON.stringify(list.map((t) => t.id)))
    assert.equal(note.inputs[0].id, 'topic')
    assert.ok(list.length > 1, 'built-in templates are listed too')
    assert.deepEqual((await host.request('template.validate', { text: TEMPLATE })).errors, [])

    const { dir: runDir } = await host.request('run.start', { templateId: 'note', inputs: { topic: 'tides' } })
    await until(() => host.launches.length === 1, 5000, 'launch')
    assert.match(host.launches[0].prompt, /Write a note on tides/)
    await host.status_('t1', 'working')
    const reported = await host.atrium('t1', ['report', 'Tides are caused by the moon.'])
    assert.equal(reported.code, 0, reported.err)
    await service.settle()
    const view = await until(async () => {
      const v = await host.request('run.get', { dir: runDir })
      return v.state === 'done' ? v : null
    }, 5000, 'run done')
    assert.equal(view.templateId, 'note')
    assert.equal(view.steps[0].id, 'write')
    assert.equal((await host.request('overview.get')).runs[0].state, 'done')
  } finally {
    await service.dispose()
  }
})

test('a project on the real core: init, task, team, board', { skip: !existsSync(wasm) && 'core not built' }, async () => {
  const dir = workspace()
  const host = new FakeHost({ workspacePath: dir })
  const core = await Core.load(wasm, ROOT)
  const service = await Service.create(host, serviceOptions(dir, core))
  try {
    const { root } = await host.request('project.init', { root: dir, brief: 'Build a calculator.' })
    assert.equal(root, dir)
    // The controller is created and started by role.start.
    await host.request('role.start', { root, role: 'controller' })
    assert.equal(host.launches.length, 1)
    assert.equal(host.launches[0].env.ATRIUM_ROLE, 'controller')
    assert.match(host.launches[0].prompt, /Build a calculator|controller/i)
    assert.match(host.launches[0].prompt, /atrium task done/)
    const task = await host.request('project.task.create', { root, title: 'Parse input', owner: 'controller' })
    assert.equal(task.title, 'Parse input')
    const moved = await host.request('project.task.move', { root, id: task.id, status: 'blocked' })
    assert.equal(moved.status, 'blocked')
    const view = await host.request('project.get', { root })
    assert.equal(view.root, dir)
    assert.equal(view.tasks.length, 1)
    assert.equal(view.org.id, 'controller')
    const team = await host.request('team.get', { root })
    assert.deepEqual(team.errors, [])
    await host.request('project.notice', { root, text: 'hello team' })
    assert.ok((await host.request('project.get', { root })).notices.some((n) => n.text.includes('hello team')))
    await host.request('project.autopilot', { root, on: true })
    assert.equal((await host.request('project.get', { root })).autopilot, true)
    await host.request('board.open', { root })
    assert.equal(host.opened.at(-1).path, '.atrium/atrium.board')
    assert.equal((await host.request('overview.get')).projects.length, 1)
  } finally {
    await service.dispose()
  }
})

test('autopilot on the real core starts a lead for the controller\'s task and marks it done from the CLI', { skip: !existsSync(wasm) && 'core not built' }, async () => {
  const dir = workspace()
  const host = new FakeHost({ workspacePath: dir })
  const core = await Core.load(wasm, ROOT)
  const service = await Service.create(host, serviceOptions(dir, core))
  try {
    await host.request('project.init', { root: dir, brief: 'Build a calculator.' })
    await host.request('role.start', { root: dir, role: 'controller' })
    const task = await host.request('project.task.create', { root: dir, title: 'Build the parser', owner: 'controller' })
    await host.request('project.autopilot', { root: dir, on: true })
    await until(() => host.launches.length === 2, 5000, 'lead launch')
    const lead = host.launches[1]
    assert.match(lead.env.ATRIUM_ROLE, /^lead:/)
    assert.equal(lead.env.ATRIUM_TASK, task.id)
    await service.settle()
    assert.equal(host.launches.length, 2, 'no second launch for the same lead')
    const view = await host.request('project.get', { root: dir })
    const node = view.org.children.find((c) => c.id === lead.env.ATRIUM_ROLE)
    assert.equal(node.terminalId, 't2')
    const done = await host.atrium('t2', ['task', 'done', task.id, 'parser works'])
    assert.equal(done.code, 0, done.err)
    assert.equal((await host.request('project.get', { root: dir })).tasks.find((t) => t.id === task.id).status, 'done')
  } finally {
    await service.dispose()
  }
})
