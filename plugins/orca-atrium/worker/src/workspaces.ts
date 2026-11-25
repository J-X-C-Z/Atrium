// Orca workspaces: which one holds a folder, the focused one, and creating worktrees.

import { isAbsolute, relative, resolve } from 'node:path'
import type { Host } from './host.js'

export interface WorkspaceRow {
  id: string
  kind: 'worktree' | 'folder'
  name: string
  path: string
  projectId?: string | null
  hostId?: string | null
  branch?: string | null
}

export interface FocusContext {
  workspaceId?: string
  branch: string
  displayName: string
  terminals: { id: string; title?: string }[]
}

export function contains(parent: string, child: string): boolean {
  const rel = relative(resolve(parent), resolve(child))
  return rel === '' || (!rel.startsWith('..') && !isAbsolute(rel))
}

export class Workspaces {
  private rows: WorkspaceRow[] = []
  private fetchedAt = 0
  focused: FocusContext | null = null

  constructor(private host: Host) {}

  async list(fresh = false): Promise<WorkspaceRow[]> {
    if (fresh || Date.now() - this.fetchedAt > 5000 || this.rows.length === 0) {
      try {
        const answer = await this.host.call<{ workspaces: WorkspaceRow[] }>('workspace.list', {})
        this.rows = answer.workspaces ?? []
        this.fetchedAt = Date.now()
      } catch (error) {
        this.host.log(`workspaces: list failed: ${String(error)}`)
      }
    }
    return this.rows
  }

  /** A workspace from the last listing, without asking Orca. */
  cached(id: string): WorkspaceRow | undefined {
    return this.rows.find((w) => w.id === id)
  }

  invalidate(): void {
    this.fetchedAt = 0
  }

  async byId(id: string): Promise<WorkspaceRow | undefined> {
    return (await this.list()).find((w) => w.id === id) ?? (await this.list(true)).find((w) => w.id === id)
  }

  /** The local workspace whose folder holds `path` most closely. */
  async forPath(path: string): Promise<WorkspaceRow | undefined> {
    const pick = (rows: WorkspaceRow[]) =>
      rows
        .filter((w) => (w.hostId ?? 'local') === 'local' && contains(w.path, path))
        .sort((a, b) => b.path.length - a.path.length)[0]
    return pick(await this.list()) ?? pick(await this.list(true))
  }

  async focusedWorkspace(): Promise<WorkspaceRow | undefined> {
    let id = this.focused?.workspaceId
    if (!id) {
      try {
        const ctx = await this.host.call<FocusContext | null>('workspace.readContext', {})
        this.focused = ctx
        id = ctx?.workspaceId
      } catch {}
    }
    return id ? this.byId(id) : undefined
  }

  /** A new worktree next to `base`, for a step or role that works on its own branch. */
  async createWorktree(base: WorkspaceRow, name: string): Promise<string> {
    if (!base.projectId) throw new Error(`workspace ${base.name} is not in a Git project`)
    const answer = await this.host.call<{ workspaceId: string }>('workspace.create', {
      projectId: base.projectId,
      ...(base.hostId && base.hostId !== 'local' ? { hostId: base.hostId } : {}),
      name: safeName(name),
      ...(base.branch ? { baseBranch: base.branch } : {})
    })
    this.invalidate()
    return answer.workspaceId
  }

  /** The path of `abs` relative to the workspace, as editor.open takes it. */
  relative(workspace: WorkspaceRow, abs: string): string {
    return relative(workspace.path, abs).split('\\').join('/')
  }
}

export function safeName(name: string): string {
  return (
    name
      .toLowerCase()
      .replace(/[^a-z0-9._-]+/g, '-')
      .replace(/^-+|-+$/g, '')
      .slice(0, 60) || 'atrium'
  )
}
