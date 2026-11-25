// The needs-you inbox: gates, questions, stalled and blocked agents, limits, errors.
// Feeds the status bar item and the UI.

import type { Host } from '../host.js'
import type { Lang, NeedItem } from '../protocol.js'
import type { Store } from '../state.js'
import { statusText } from '../i18n.js'

export type NeedResolver = (need: NeedItem, answer?: string) => Promise<void> | void

export class Needs {
  private resolvers = new Map<string, NeedResolver>()
  private running = () => 0

  constructor(
    private host: Host,
    private store: Store,
    private lang: () => Lang
  ) {}

  list(): NeedItem[] {
    return [...this.store.data.needs].sort((a, b) => a.createdAt - b.createdAt)
  }

  get(id: string): NeedItem | undefined {
    return this.store.data.needs.find((n) => n.id === id)
  }

  /** Per kind: what resolving a need does (send an answer, decide a gate…). */
  onResolve(kind: NeedItem['kind'], resolver: NeedResolver): void {
    this.resolvers.set(kind, resolver)
  }

  countRunning(fn: () => number): void {
    this.running = fn
  }

  /** Adds or replaces by id. Notifies only for a new item. */
  add(need: NeedItem, notify = true): void {
    const list = this.store.data.needs
    const at = list.findIndex((n) => n.id === need.id)
    if (at >= 0) {
      list[at] = { ...need, createdAt: list[at]!.createdAt }
    } else {
      list.push(need)
      if (notify) void this.notify(need)
    }
    this.store.save()
    this.changed()
  }

  remove(id: string): boolean {
    const before = this.store.data.needs.length
    this.store.data.needs = this.store.data.needs.filter((n) => n.id !== id)
    if (this.store.data.needs.length === before) return false
    this.store.save()
    this.changed()
    return true
  }

  removeWhere(pred: (n: NeedItem) => boolean): void {
    const before = this.store.data.needs.length
    this.store.data.needs = this.store.data.needs.filter((n) => !pred(n))
    if (this.store.data.needs.length !== before) {
      this.store.save()
      this.changed()
    }
  }

  async resolve(id: string, answer?: string): Promise<void> {
    const need = this.get(id)
    if (!need) return
    const resolver = this.resolvers.get(need.kind)
    if (resolver) await resolver(need, answer)
    this.remove(id)
  }

  forRoot(root: string): number {
    return this.store.data.needs.filter((n) => n.source.kind === 'project' && n.source.root === root).length
  }

  /** Status bar item and posts. */
  changed(): void {
    const count = this.store.data.needs.length
    this.host.post('needs.changed', { count })
    this.host.post('overview.changed')
    void this.updateStatusBar()
  }

  async updateStatusBar(): Promise<void> {
    const count = this.store.data.needs.length
    const running = this.running()
    try {
      await this.host.call('statusBar.update', {
        id: 'needs',
        text: statusText(this.lang(), count, running),
        tone: count > 0 ? 'warning' : running > 0 ? 'accent' : 'neutral',
        visible: true
      })
    } catch {
      // Desktop only; browser clients and older Orca have no status bar.
    }
  }

  private async notify(need: NeedItem): Promise<void> {
    try {
      await this.host.call('notifications.show', { title: need.title, ...(need.detail ? { body: need.detail.slice(0, 300) } : {}) })
    } catch {}
  }
}
