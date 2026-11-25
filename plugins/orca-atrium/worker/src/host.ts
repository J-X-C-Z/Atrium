// The slice of Orca's worker API that Atrium uses, so tests can run against a fake.

export interface Disposable {
  dispose(): void
}

export type HookNext = (event?: unknown) => unknown
export type HookHandler = (event: any, next: HookNext) => unknown

export interface Host {
  call<T = any>(method: string, params?: unknown): Promise<T>
  has(method: string): boolean
  on(event: string, handler: (payload: any) => void | Promise<void>): Disposable
  hook(name: string, handler: HookHandler): Disposable
  handle(name: string, handler: (input: any) => unknown): Disposable
  post(name: string, payload?: unknown): void
  command(id: string, handler: (args: unknown) => unknown): Disposable
  storageGet<T = unknown>(key: string): Promise<T | null>
  storageSet(key: string, value: unknown): Promise<void>
  settings(): Promise<Record<string, unknown>>
  onSettings(listener: () => void): Disposable
  log(message: string): void
}

/** The `orca` object handed to `activate`, as far as we use it. */
export interface OrcaWorker {
  host: { call(method: string, params?: unknown): Promise<any>; has(method: string): boolean }
  events: { on(event: string, handler: (payload: any) => void | Promise<void>): Disposable }
  hooks: { on(hook: string, handler: HookHandler): Disposable }
  panels: { handle(name: string, handler: (input: any, ctx: unknown) => unknown): Disposable; post(name: string, payload?: unknown): void }
  commands: { register(id: string, handler: (args: unknown, ctx: unknown) => unknown): Disposable }
  storage: { get(key: string): Promise<unknown>; set(key: string, value: unknown): Promise<void> }
  settings?: { onDidChange?(listener: (change: unknown) => void): Disposable }
  log(message: string): void
}

export function orcaHost(orca: OrcaWorker): Host {
  return {
    call: (method, params) => orca.host.call(method, params ?? {}),
    has: (method) => {
      try {
        return orca.host.has(method)
      } catch {
        return false
      }
    },
    on: (event, handler) => orca.events.on(event, handler),
    hook: (name, handler) => orca.hooks.on(name, handler),
    handle: (name, handler) => orca.panels.handle(name, (input) => handler(input)),
    post: (name, payload) => {
      try {
        orca.panels.post(name, payload ?? null)
      } catch (error) {
        orca.log(`post ${name}: ${String(error)}`)
      }
    },
    command: (id, handler) => orca.commands.register(id, (args) => handler(args)),
    storageGet: async <T>(key: string) => ((await orca.storage.get(key)) ?? null) as T | null,
    storageSet: (key, value) => orca.storage.set(key, value),
    settings: async () => ((await orca.host.call('settings.get', {})) as { settings?: Record<string, unknown> } | null)?.settings ?? {},
    onSettings: (listener) => orca.settings?.onDidChange?.(() => listener()) ?? { dispose() {} },
    log: (message) => orca.log(message)
  }
}

/** Host error with an Orca error code, so panels see it. */
export class HostError extends Error {
  constructor(
    readonly code: string,
    message: string
  ) {
    super(message)
  }
}
