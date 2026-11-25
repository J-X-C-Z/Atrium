// Small state kept in the plugin's own storage, so a worker restart picks up where it left off.
// Run and project state live in their own files (through the core).

import { randomBytes } from 'node:crypto'
import type { Host } from './host.js'
import type { NeedItem, Quota } from './protocol.js'

export type BindingKind = 'step' | 'review' | 'role'

/** An agent we launched and what it works on. */
export interface Binding {
  launchId: string
  terminalId: string | null
  workspaceId: string
  agent: string
  kind: BindingKind
  dir?: string
  step?: string
  item?: string
  checkIndex?: number
  root?: string
  role?: string
  task?: string
  member?: string
  standingInFor?: string
  startedAt: number
  reported: boolean
  nudged: boolean
  /** Superseded, stopped or gone: no longer drives anything. */
  ended?: boolean
}

export interface RunMeta {
  dir: string
  workspaceId: string
  templateId: string
}

export interface ProjectMeta {
  root: string
  workspaceId?: string
}

export interface Persisted {
  token: string
  projects: ProjectMeta[]
  runs: RunMeta[]
  needs: NeedItem[]
  bindings: Binding[]
  quotas: Record<string, Quota>
}

const KEY = 'atrium.state'

export class Store {
  data: Persisted
  private timer: NodeJS.Timeout | null = null
  private writing: Promise<void> = Promise.resolve()

  private constructor(
    private host: Host,
    data: Persisted
  ) {
    this.data = data
  }

  static async load(host: Host): Promise<Store> {
    let raw: Partial<Persisted> | null = null
    try {
      raw = await host.storageGet<Partial<Persisted>>(KEY)
    } catch (error) {
      host.log(`state: could not read storage: ${String(error)}`)
    }
    const data: Persisted = {
      token: typeof raw?.token === 'string' && raw.token ? raw.token : randomBytes(18).toString('hex'),
      projects: Array.isArray(raw?.projects) ? raw!.projects : [],
      runs: Array.isArray(raw?.runs) ? raw!.runs : [],
      needs: Array.isArray(raw?.needs) ? raw!.needs : [],
      bindings: Array.isArray(raw?.bindings) ? raw!.bindings : [],
      quotas: raw?.quotas && typeof raw.quotas === 'object' ? raw.quotas : {}
    }
    const store = new Store(host, data)
    if (!raw?.token) store.save()
    return store
  }

  /** Coalesced write. */
  save(): void {
    if (this.timer) return
    this.timer = setTimeout(() => {
      this.timer = null
      void this.flush()
    }, 50)
  }

  async flush(): Promise<void> {
    if (this.timer) {
      clearTimeout(this.timer)
      this.timer = null
    }
    const snapshot = JSON.parse(JSON.stringify(this.data))
    this.writing = this.writing
      .then(() => this.host.storageSet(KEY, snapshot))
      .catch((error) => this.host.log(`state: could not write storage: ${String(error)}`))
    await this.writing
  }

  // ---------- bindings ----------

  live(): Binding[] {
    return this.data.bindings.filter((b) => !b.ended)
  }

  byTerminal(terminalId: string): Binding | undefined {
    return this.live().find((b) => b.terminalId === terminalId)
  }

  byLaunch(launchId: string): Binding | undefined {
    return this.data.bindings.find((b) => b.launchId === launchId)
  }

  forStep(dir: string, step: string, item?: string): Binding | undefined {
    return this.live().find((b) => b.kind === 'step' && b.dir === dir && b.step === step && (b.item ?? '') === (item ?? ''))
  }

  forRole(root: string, role: string): Binding | undefined {
    return this.live().find((b) => b.kind === 'role' && b.root === root && b.role === role)
  }

  addBinding(b: Binding): void {
    this.data.bindings.push(b)
    // Keep the history short: ended bindings beyond the newest 200 go.
    const ended = this.data.bindings.filter((x) => x.ended)
    if (ended.length > 200) {
      const drop = new Set(ended.slice(0, ended.length - 200))
      this.data.bindings = this.data.bindings.filter((x) => !drop.has(x))
    }
    this.save()
  }

  end(b: Binding): void {
    b.ended = true
    this.save()
  }
}
