// A small in-memory stand-in for the wasm core: one two-step workflow and one project.
import { mkdirSync, writeFileSync } from 'node:fs'

export class FakeCore {
  constructor() {
    this.runs = new Map()
    this.projects = new Map()
    this.log = []
  }

  addProject(root, extra = {}) {
    this.projects.set(root, {
      name: 'demo',
      autopilot: false,
      notices: [],
      tasks: [{ id: 't1', title: 'Build the API', status: 'ready', owner: 'lead:api' }],
      ...extra
    })
    mkdirSync(`${root}/.atrium`, { recursive: true })
    writeFileSync(`${root}/.atrium/project.json`, '{}')
    return this.projects.get(root)
  }

  call(op, args) {
    this.log.push({ op, args })
    const fn = this[`op_${op.replaceAll('.', '_')}`]
    if (!fn) throw new Error(`fake core: unknown op ${op}`)
    return fn.call(this, args)
  }

  // ---------- workflow: draft (agent) -> check (command) -> gate ----------

  op_workflow_templates() {
    return [{ id: 'demo', name: 'Demo workflow', source: 'builtin', steps: 3, inputs: [{ id: 'topic', label: 'Topic', kind: 'text', required: true }] }]
  }
  op_workflow_run_start({ template, inputs, dir }) {
    mkdirSync(dir, { recursive: true })
    this.runs.set(dir, { template, inputs, stage: 'draft', attempt: 1, reports: [], decisions: [], state: 'running', checks: [] })
    return { dir }
  }
  run(dir) {
    const r = this.runs.get(dir)
    if (!r) throw new Error(`no run ${dir}`)
    return r
  }
  op_workflow_run_next({ dir }) {
    const r = this.run(dir)
    if (r.state === 'paused' || r.state === 'cancelled') return { state: r.state, effects: [] }
    return { state: r.state, effects: this.effects(r) }
  }
  effects(r) {
    switch (r.stage) {
      case 'draft':
        return [{ kind: 'launch', step: 'draft', prompt: `Write about ${r.inputs.topic} (attempt ${r.attempt})`, agent: 'claude' }]
      case 'check':
        return [{ kind: 'check', step: 'draft', index: 0, check: { kind: 'command', run: r.failCheck ? 'exit 3' : 'exit 0' } }]
      case 'gate':
        return [{ kind: 'gate', step: 'gate', title: 'Approve the draft?', choices: [{ id: 'approve', label: 'Approve' }], sendBack: [{ step: 'draft', label: 'Draft' }] }]
      default:
        return [{ kind: 'done', state: 'done' }]
    }
  }
  op_workflow_run_report({ dir, step, answer }) {
    const r = this.run(dir)
    if (step !== 'draft' || r.stage !== 'draft') throw new Error(`step ${step} is not waiting for a report`)
    r.reports.push(answer)
    r.stage = 'check'
  }
  op_workflow_run_check_result({ dir, result }) {
    const r = this.run(dir)
    r.checks.push(result)
    if (result.passed) r.stage = 'gate'
    else {
      r.stage = 'draft'
      r.attempt++
    }
  }
  op_workflow_run_decide({ dir, step, choice, send_back_to, note }) {
    const r = this.run(dir)
    r.decisions.push({ at: 1, step, choice, sendBackTo: send_back_to, note })
    if (send_back_to) {
      r.stage = 'draft'
      r.attempt++
    } else {
      r.stage = 'done'
      r.state = 'done'
    }
  }
  op_workflow_run_retry({ dir }) {
    const r = this.run(dir)
    if (r.stage === 'check') r.stage = 'draft'
  }
  op_workflow_run_pause({ dir }) {
    this.run(dir).state = 'paused'
  }
  op_workflow_run_resume({ dir }) {
    this.run(dir).state = 'running'
  }
  op_workflow_run_cancel({ dir }) {
    this.run(dir).state = 'cancelled'
  }
  op_workflow_run_load({ dir }) {
    const r = this.run(dir)
    const status = (stage) => (r.stage === stage ? (stage === 'gate' ? 'waiting' : 'running') : r.stage === 'done' || ['check', 'gate'].indexOf(r.stage) > ['check', 'gate'].indexOf(stage) ? 'passed' : 'pending')
    return {
      templateId: r.template,
      templateName: 'Demo workflow',
      state: r.state,
      startedAt: 1,
      progress: { done: r.stage === 'done' ? 2 : r.stage === 'gate' ? 1 : 0, total: 2 },
      currentStep: r.stage === 'gate' ? 'gate' : 'draft',
      inputs: r.inputs,
      steps: [
        { id: 'draft', title: 'Draft', needs: [], kind: 'agent', status: r.stage === 'draft' ? 'running' : r.stage === 'check' ? 'checking' : 'passed', attempts: r.reports.map((report, i) => ({ n: i + 1, startedAt: 1, report, checks: [] })) },
        { id: 'gate', title: 'Approve', needs: ['draft'], kind: 'gate', status: status('gate'), attempts: [], sendBack: ['draft'] }
      ],
      decisions: r.decisions,
      experience: []
    }
  }
  op_workflow_template_validate({ text }) {
    return text.includes('steps') ? { errors: [] } : { errors: [{ line: 1, message: 'no steps' }] }
  }

  // ---------- project ----------

  p(root) {
    const p = this.projects.get(root)
    if (!p) throw new Error(`${root} is not an Atrium project`)
    return p
  }
  op_project_init({ root }) {
    if (!this.projects.has(root)) this.addProject(root, { tasks: [] })
    return { root }
  }
  op_project_open({ root }) {
    const p = this.p(root)
    return {
      ...p,
      org: {
        id: 'controller',
        role: { kind: 'controller' },
        children: [{ id: 'lead:api', role: { kind: 'lead', name: 'api' }, member: 'alice', agent: 'claude', children: [] }]
      },
      team: { members: this.members() }
    }
  }
  op_project_autopilot_set({ root, on }) {
    this.p(root).autopilot = on
  }
  op_project_task_move({ root, id, status }) {
    const t = this.p(root).tasks.find((x) => x.id === id)
    if (!t) throw new Error(`no task ${id}`)
    t.status = status
    return t
  }
  op_project_notice_add({ root, text }) {
    this.p(root).notices.push({ at: 1, text })
  }
  launchFor(role, task, quotas = {}) {
    const spent = Boolean(quotas.alice)
    return {
      kind: 'launch',
      role,
      stream: role.replace('lead:', ''),
      agent: spent ? 'codex' : 'claude',
      member: spent ? 'bob' : 'alice',
      prompt: `Kickoff for ${role}${task ? ` on ${task}` : ''}`,
      cwd: this.root,
      worktree: false,
      ...(task ? { task } : {}),
      ...(spent ? { standingInFor: 'alice' } : {})
    }
  }
  op_project_role_start({ root, role, quotas }) {
    this.root = root
    const t = this.p(root).tasks.find((x) => x.owner === role && x.status !== 'done')
    return { launch: this.launchFor(role, t?.id, quotas) }
  }
  op_project_bind({ root, role, thread }) {
    this.p(root).bound = { ...(this.p(root).bound ?? {}), [role]: thread }
  }
  op_project_stream_failed() {
    return { parent: null }
  }
  op_project_next({ root, live, quotas }) {
    this.root = root
    const p = this.p(root)
    const actions = p.tasks
      .filter((t) => t.status === 'ready' && t.owner && !live.includes(t.owner))
      .map((t) => this.launchFor(t.owner, t.id, quotas))
    for (const a of actions) p.tasks.find((t) => t.id === a.task).status = 'in_progress'
    return { actions }
  }
  members() {
    return [
      { name: 'alice', agent: 'claude', standIns: ['bob'] },
      { name: 'bob', agent: 'codex' }
    ]
  }
  op_team_read() {
    return { members: this.members(), errors: [] }
  }
  op_team_write() {
    return { errors: [] }
  }
  op_team_pick({ wanted, quotas }) {
    if (quotas && quotas[wanted]) return { pick: { member: 'bob', agent: 'codex', model: null, insteadOf: { member: wanted, reason: 'spent' } } }
    const m = this.members().find((x) => x.name === wanted)
    return { pick: { member: wanted, agent: m?.agent } }
  }
}
