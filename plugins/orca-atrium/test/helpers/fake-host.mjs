// A stand-in for Orca's worker API: records calls, runs hooks, delivers events.
import { spawn } from 'node:child_process'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

export const BIN = join(fileURLToPath(new URL('../../bin', import.meta.url)), 'atrium')

export class FakeHost {
  constructor({ workspacePath, settings = {} }) {
    this.workspacePath = workspacePath
    this.storage = new Map()
    this.settingsValue = { language: 'en', defaultAgent: 'claude', maxParallel: 4, ...settings }
    this.hooks = new Map()
    this.events = new Map()
    this.handlers = new Map()
    this.commands = new Map()
    this.posts = []
    this.calls = []
    this.launches = []
    this.sent = []
    this.closed = []
    this.notifications = []
    this.status = []
    this.opened = []
    this.logs = []
    this.terminalText = new Map()
    this.agentsListed = []
    this.nextTerminal = 1
  }

  has() {
    return true
  }

  async call(method, params = {}) {
    this.calls.push({ method, params })
    switch (method) {
      case 'workspace.list':
        return { workspaces: [{ id: 'ws1', kind: 'folder', name: 'proj', path: this.workspacePath, hostId: 'local', projectId: null, branch: null }], truncated: false }
      case 'workspace.readContext':
        return { workspaceId: 'ws1', branch: 'main', displayName: 'proj', terminals: [] }
      case 'agent.launch': {
        const terminalId = `t${this.nextTerminal++}`
        const event = { agent: params.agent, workspaceId: params.workspaceId, workspaceKind: 'folder', terminalId, command: params.agent, env: { PATH: '/usr/bin:/bin' } }
        const hook = this.hooks.get('agent.launch.before')
        const decided = hook ? await hook(event, (e) => ({ next: e })) : undefined
        const env = decided?.next?.env ?? event.env
        this.launches.push({ terminalId, agent: params.agent, workspaceId: params.workspaceId, prompt: params.prompt, env, hooked: Boolean(decided?.next) })
        return { terminalId }
      }
      case 'agents.list':
        return { agents: this.agentsListed }
      case 'terminal.sendText':
        this.sent.push(params)
        return { accepted: true }
      case 'terminal.read':
        return { lines: this.terminalText.get(params.terminalId) ?? [], truncated: false }
      case 'terminal.close':
        this.closed.push(params.terminalId)
        return { closed: true }
      case 'terminal.focus':
        return { focused: true }
      case 'notifications.show':
        this.notifications.push(params)
        return { delivered: true }
      case 'statusBar.update':
        this.status.push(params)
        return { ok: true }
      case 'editor.open':
        this.opened.push(params)
        return { opened: true }
      default:
        throw Object.assign(new Error(`fake host: ${method}`), { code: 'unknown_method' })
    }
  }

  on(event, handler) {
    this.events.set(event, handler)
    return { dispose() {} }
  }
  hook(name, handler) {
    this.hooks.set(name, handler)
    return { dispose() {} }
  }
  handle(name, handler) {
    this.handlers.set(name, handler)
    return { dispose() {} }
  }
  command(id, handler) {
    this.commands.set(id, handler)
    return { dispose() {} }
  }
  post(name, payload) {
    this.posts.push({ name, payload })
  }
  async storageGet(key) {
    return this.storage.has(key) ? structuredClone(this.storage.get(key)) : null
  }
  async storageSet(key, value) {
    this.storage.set(key, structuredClone(value))
  }
  async settings() {
    return this.settingsValue
  }
  onSettings() {
    return { dispose() {} }
  }
  log(message) {
    this.logs.push(message)
  }

  /** A panel request, as the UI sends it. */
  request(name, params) {
    const handler = this.handlers.get(name)
    if (!handler) throw new Error(`no handler ${name}`)
    return handler(params)
  }
  async emit(event, payload) {
    await this.events.get(event)?.(payload)
  }
  status_(terminalId, state, workspaceId = 'ws1') {
    return this.emit('agent.status.changed', { terminalId, workspaceId, state, paneKey: `p-${terminalId}`, receivedAt: Date.now() })
  }

  /** Runs `atrium <args>` the way an agent in terminal `terminalId` would. */
  atrium(terminalId, args, { stdin } = {}) {
    const launch = this.launches.find((l) => l.terminalId === terminalId)
    const env = { ...process.env, ...launch.env, ORCA_TERMINAL_HANDLE: terminalId }
    return new Promise((resolve) => {
      const child = spawn(process.execPath, [BIN, ...args], { env, stdio: ['pipe', 'pipe', 'pipe'] })
      let out = ''
      let err = ''
      child.stdout.on('data', (d) => (out += d))
      child.stderr.on('data', (d) => (err += d))
      child.on('close', (code) => resolve({ code, out: out.trim(), err: err.trim() }))
      child.stdin.end(stdin ?? '')
    })
  }
}

export async function until(fn, ms = 4000, what = 'condition') {
  const end = Date.now() + ms
  for (;;) {
    const v = await fn()
    if (v) return v
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`)
    await new Promise((r) => setTimeout(r, 10))
  }
}
