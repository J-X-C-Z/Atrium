// Thin, typed access to the frame's `window.orca` (see Orca's plugin-isolated-frames.md).
import type { Requests, RequestName, Posts, PostName } from '../../worker/src/protocol'

export interface Disposable {
  dispose(): void
}

export interface OrcaDocument {
  version: number | string
  readOnly: boolean
  content: { kind: 'text'; text: string } | { kind: 'bytes'; bytes: ArrayBuffer }
  fileType?: string
}

export interface DocumentSession<T> {
  value: T
  readOnly: boolean
  onReplace(cb: (value: T) => void): Disposable | void
  change(value: T): void
  flush(): Promise<void>
}

export interface OrcaFrame {
  api?: { major: number; minor: number }
  locale?: string
  host: {
    call(method: string, params?: unknown, opts?: { signal?: AbortSignal }): Promise<unknown>
    has(method: string): boolean
  }
  on(event: string, cb: (payload: any) => void): Disposable
  plugin?: {
    request(name: string, input?: unknown, opts?: { timeoutMs?: number }): Promise<unknown>
    on(name: string, cb: (payload: any) => void): Disposable
  }
  document?: {
    get(): Promise<OrcaDocument>
    onDidChange(cb: (doc: OrcaDocument) => void): Disposable | void
    applyEdit(edit: { baseVersion: OrcaDocument['version']; content: { kind: 'text'; text: string } }): Promise<unknown>
    session?<T>(opts: {
      decode?: (content: OrcaDocument['content']) => T
      encode?: (value: T) => { kind: 'text'; text: string }
      merge?: (base: T, outside: T, mine: T) => T
      debounceMs?: number
    }): Promise<DocumentSession<T>>
  }
}

declare global {
  interface Window {
    orca?: OrcaFrame
  }
}

export class RequestError extends Error {
  code: string
  constructor(code: string, message: string) {
    super(message)
    this.code = code
  }
}

function frame(): OrcaFrame {
  const o = window.orca
  if (!o) throw new RequestError('unavailable', 'window.orca is missing')
  return o
}

export function hasOrca(): boolean {
  return typeof window !== 'undefined' && !!window.orca
}

/** Ask the plugin worker. Uses `orca.plugin.request` when present, else `host.call('plugin.request')`. */
export async function request<N extends RequestName>(name: N, params?: Requests[N]['params']): Promise<Requests[N]['result']> {
  const o = frame()
  try {
    if (o.plugin?.request) return (await o.plugin.request(name, params ?? {})) as Requests[N]['result']
    const res = (await o.host.call('plugin.request', { name, input: params ?? {} })) as { output: unknown }
    return res?.output as Requests[N]['result']
  } catch (err) {
    const e = err as { code?: string; message?: string }
    throw new RequestError(e?.code ?? 'action_failed', e?.message ?? String(err))
  }
}

/** Listen to a worker post (`orca.panels.post`). */
export function onPost<P extends PostName>(name: P, cb: (payload: Posts[P]) => void): Disposable {
  const o = window.orca
  if (!o) return { dispose() {} }
  if (o.plugin?.on) return o.plugin.on(name, cb)
  return o.on(name, cb)
}

export function onTheme(cb: () => void): Disposable {
  const o = window.orca
  if (!o) return { dispose() {} }
  try {
    return o.on('theme', cb)
  } catch {
    return { dispose() {} }
  }
}

export function locale(): string {
  return window.orca?.locale ?? navigator.language ?? 'en'
}

export function orcaDocument() {
  return window.orca?.document
}
