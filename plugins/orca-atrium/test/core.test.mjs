// The wasm core, built and loaded the way the worker loads it: a project, a team, and a small
// workflow run end to end with scripted agent reports and a gate decision that sends work back.
// Build the core first: sh scripts/build-core.sh

import assert from 'node:assert/strict'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { after, before, describe, test } from 'node:test'
import { fileURLToPath } from 'node:url'
import { Core, CoreError, ROOT } from '../worker/src/core.ts'

const wasm = join(dirname(fileURLToPath(import.meta.url)), '..', 'core', 'atrium_core.wasm')
const TEMPLATE = `
id = "essay"
name = "Essay"

[[inputs]]
key = "topic"
label = "Topic"
kind = "text"
required = true

[[steps]]
id = "outline"
title = "Outline"
prompt = "Outline an essay on {{input.topic}}."
outputs = ["outline.md"]

[[steps]]
id = "draft"
title = "Draft"
prompt = "Write the essay."
outputs = ["essay.md"]
checks = [{ kind = "contains", path = "essay.md", pattern = "(?i)conclusion" }, { kind = "command", run = "typeset essay.md" }]

[[steps]]
id = "review"
title = "Review"
prompt = "Review the essay."
gate = true
fixes = "draft"
`

let root
let core

before(async () => {
  assert.ok(existsSync(wasm), 'build the core first: sh scripts/build-core.sh')
  root = realpathSync(mkdtempSync(join(tmpdir(), 'atrium-core-')))
  core = await Core.load(wasm, ROOT)
})

after(() => rmSync(root, { recursive: true, force: true }))

describe('the wasm core', () => {
  test('answers ops and reports errors with their message', () => {
    const info = core.call('core.info', {})
    assert.equal(info.wasi, true)
    assert.throws(
      () => core.call('nope', {}),
      (error) => error instanceof CoreError && error.message === 'unknown op nope'
    )
    assert.throws(() => core.call('project.open', { root: join(root, 'missing') }), /not an Atrium project|No such file|missing/)
  })

  test('a project is made, opened and changed', () => {
    const project = join(root, 'proj')
    mkdirSync(project)
    core.call('project.init', { root: project, brief: 'Build a tea shop site.' })
    for (const file of ['project.json', 'brief.md', 'notices.md']) assert.ok(existsSync(join(project, '.atrium', file)), file)

    const started = core.call('project.role.start', { root: project, role: 'controller', agent: 'claude', goal: 'Plan the site' })
    assert.equal(started.launch.role, 'controller')
    assert.match(started.launch.prompt, /Plan the site/)

    const lead = core.call('project.role.start', { root: project, role: 'lead:web', agent: 'codex' })
    assert.equal(lead.launch.role, 'lead:web')
    const task = core.call('project.task.create', { root: project, title: 'Build the menu page', owner: 'lead:web' })
    assert.equal(task.id, 'web/build-the-menu-page')
    assert.equal(task.status, 'ready')
    core.call('project.task.move', { root: project, id: task.id, status: 'in_progress' })
    core.call('project.notice.add', { root: project, text: 'Use plain CSS.' })
    core.call('project.autopilot.set', { root: project, on: true })

    const view = core.call('project.open', { root: project })
    assert.equal(view.name, 'proj')
    assert.equal(view.autopilot, true)
    assert.equal(view.brief.trim(), 'Build a tea shop site.')
    assert.equal(view.org.id, 'controller')
    assert.deepEqual(view.org.children.map((c) => c.id), ['lead:web'])
    assert.equal(view.tasks[0].status, 'in_progress')
    assert.equal(view.notices[0].text, 'Use plain CSS.')
    assert.deepEqual(view.progress, { done: 0, total: 1 })

    const next = core.call('project.next', { root: project, live: ['controller'] })
    assert.ok(Array.isArray(next.actions))
    // The file is the app's: its own reader accepts what the core wrote.
    assert.match(readFileSync(join(project, '.atrium', 'project.json'), 'utf8'), /"autopilot"/)
  })

  test('a team is read, written and used to pick stand-ins', () => {
    const project = join(root, 'proj')
    const fresh = core.call('team.read', { root: project })
    assert.equal(fresh.exists, false)
    assert.ok(fresh.members.length > 0)
    const members = [
      { name: 'claude', agent: 'claude', notes: 'plans', standIns: ['gpt'] },
      { name: 'gpt', agent: 'codex', model: 'gpt-5' }
    ]
    assert.deepEqual(core.call('team.write', { root: project, members }).errors, [])
    const back = core.call('team.read', { root: project })
    assert.equal(back.exists, true)
    assert.deepEqual(back.members.map((m) => m.name), ['claude', 'gpt'])
    const bad = core.call('team.write', { root: project, members: [{ name: 'a', agent: 'claude', standIns: ['ghost'] }] })
    assert.match(bad.errors[0].message, /ghost/)
    const pick = core.call('team.pick', { root: project, wanted: 'claude', quotas: { claude: 5000 }, now: 1000 })
    assert.equal(pick.pick.member, 'gpt')
    assert.equal(pick.pick.insteadOf.reason, 'spent')
    assert.match(core.call('team.roster', { root: project, now: 1000 }).text, /gpt/)
  })

  test('a workflow runs end to end with scripted agents and a send-back at the gate', () => {
    const dir = join(root, 'run')
    const listed = core.call('workflow.templates', { dirs: [] })
    assert.ok(listed.some((t) => t.id === 'mathmodel' && t.source === 'builtin'), 'the CUMCM template is built in')
    assert.deepEqual(core.call('workflow.template.validate', { text: TEMPLATE }).errors, [])
    assert.throws(() => core.call('workflow.run.start', { template: 'essay', text: TEMPLATE, inputs: {}, dir }), /Topic/)
    core.call('workflow.run.start', { template: 'essay', text: TEMPLATE, inputs: { topic: 'tea' }, dir })

    // What each scripted agent does when its step is launched.
    const work = {
      outline: () => writeFileSync(join(dir, 'outline.md'), '1. Leaves'),
      draft: (n) => writeFileSync(join(dir, 'essay.md'), n === 1 ? 'Tea is nice.' : 'Tea is nice. In conclusion, tea.')
    }
    const launched = {}
    const prompts = []
    let sentBack = false
    let typesetRuns = 0
    for (let pass = 0; pass < 40; pass++) {
      const { state, effects } = core.call('workflow.run.next', { dir })
      if (effects.some((e) => e.kind === 'done')) {
        assert.equal(state, 'done')
        break
      }
      let progressed = false
      for (const e of effects) {
        if (e.kind === 'launch') {
          launched[e.step] = (launched[e.step] ?? 0) + 1
          prompts.push([e.step, e.prompt])
          work[e.step]?.(launched[e.step])
          core.call('workflow.run.report', { dir, step: e.step, answer: `${e.step} done (${launched[e.step]})` })
          progressed = true
        } else if (e.kind === 'check') {
          typesetRuns++
          assert.equal(e.check.kind, 'command')
          core.call('workflow.run.check_result', { dir, step: e.step, index: e.index, result: { what: e.check.what, passed: true, detail: '' } })
          progressed = true
        } else if (e.kind === 'gate') {
          assert.equal(e.step, 'review')
          if (!sentBack) {
            sentBack = true
            assert.ok(e.sendBack.some((s) => s.step === 'outline'))
            core.call('workflow.run.decide', { dir, step: 'review', choice: 'approve', send_back_to: 'outline', note: 'Add a history section.' })
          } else {
            core.call('workflow.run.decide', { dir, step: 'review', choice: 'approve' })
          }
          progressed = true
        }
      }
      assert.ok(progressed, `stuck: ${JSON.stringify(effects)}`)
    }

    // The first draft lacked a conclusion: it went again with the reason, before any command ran for it.
    const drafts = prompts.filter(([step]) => step === 'draft')
    assert.match(drafts[1][1], /conclusion/i)
    // The send-back reopened the outline with the note, and the draft ran again after it.
    const outlines = prompts.filter(([step]) => step === 'outline')
    assert.equal(outlines.length, 2)
    assert.match(outlines[1][1], /Add a history section\./)
    assert.equal(launched.draft, 3)
    assert.ok(typesetRuns >= 2)

    const view = core.call('workflow.run.load', { dir })
    assert.equal(view.state, 'done')
    assert.deepEqual(view.progress, { done: 3, total: 3 })
    assert.deepEqual(view.steps.map((s) => s.status), ['passed', 'passed', 'passed'])
    assert.equal(view.steps[1].attempts[0].checks.some((c) => c.ok === false), true)
    assert.equal(view.decisions.at(-1).step, 'review')
    assert.ok(view.decisions.some((d) => d.sendBackTo === 'outline' && d.note === 'Add a history section.'))
    assert.ok(existsSync(join(dir, '.atrium', 'run.json')))
  })
})
