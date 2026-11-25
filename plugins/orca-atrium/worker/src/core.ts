// Loads the wasm core (crates/atrium-orca-core) with node:wasi and talks to it in JSON.
//
// ABI: `alloc(len) -> ptr`, `dealloc(ptr, len)`, `call(ptr, len) -> u64` (ptr << 32 | len of the
// reply). A request is `{"op", "args"}`, a reply `{"ok": ...}` or `{"err": "..."}`.
//
// Kept to syntax Node's type stripping accepts (no enums, no parameter properties), so the
// tests can import this file directly.

import { existsSync, readFileSync } from 'node:fs'
import { WASI } from 'node:wasi'
import type { CoreApi } from './coreapi.js'

/** A failed op: the core's own message. */
export class CoreError extends Error {
  readonly op: string
  constructor(op: string, message: string) {
    super(message)
    this.name = 'CoreError'
    this.op = op
  }
}

// The worker's tsconfig has no DOM lib, so WebAssembly is typed here, as far as we use it.
interface WasmModule {
  readonly __wasm?: never
}
interface WasmApi {
  compile(bytes: Uint8Array): Promise<WasmModule>
  Instance: new (module: WasmModule, imports: object) => { exports: object }
}
const WA = (globalThis as unknown as { WebAssembly: WasmApi }).WebAssembly

interface Exports {
  memory: { buffer: ArrayBuffer }
  alloc(len: number): number
  dealloc(ptr: number, len: number): void
  call(ptr: number, len: number): bigint
}

const encoder = new TextEncoder()
const decoder = new TextDecoder()

/**
 * Folders the core may read and write, as host paths. The core sees them under the same
 * path, so ops take ordinary absolute paths. Missing folders are left out. The set is fixed
 * for the life of the instance: load a new `Core` to reach more.
 *
 * wasi-libc resolves absolute paths from the root folder, and the core's file checks
 * (`exists`, `contains`, globs) and run folders rely on that. Pass `ROOT` unless you have
 * a reason not to; the core only touches paths the worker gives it.
 */
export type Preopens = string[] | Record<string, string>

/** The whole file system, seen under its own paths (POSIX). */
export const ROOT: Record<string, string> = { '/': '/' }

function mapping(preopens: Preopens): Record<string, string> {
  const out: Record<string, string> = {}
  if (Array.isArray(preopens)) for (const p of preopens) out[p] = p
  else Object.assign(out, preopens)
  for (const guest of Object.keys(out)) if (!existsSync(out[guest]!)) delete out[guest]
  return out
}

export class Core implements CoreApi {
  private readonly module: WasmModule
  private readonly preopens: Record<string, string>
  private exports: Exports | null = null
  private busy = false

  private constructor(module: WasmModule, preopens: Record<string, string>) {
    this.module = module
    this.preopens = preopens
  }

  /** Compiles the wasm at `wasmPath`; the preopened folders are what the core may touch. */
  static async load(wasmPath: string, preopens: Preopens = ROOT): Promise<Core> {
    const module = await WA.compile(readFileSync(wasmPath))
    const core = new Core(module, mapping(preopens))
    core.instance()
    return core
  }

  /** The folders the core can reach (guest path → host path). */
  get folders(): Readonly<Record<string, string>> {
    return this.preopens
  }

  private instance(): Exports {
    if (this.exports) return this.exports
    const wasi = new WASI({ version: 'preview1', args: ['atrium-core'], env: {}, preopens: this.preopens, returnOnExit: true })
    const instance = new WA.Instance(this.module, wasi.getImportObject())
    wasi.initialize(instance)
    this.exports = instance.exports as unknown as Exports
    return this.exports
  }

  /**
   * Runs one op. Calls never overlap: the core is synchronous, and a call made from inside
   * another (there is no way to) is refused. A crash inside the core (a trap) throws and the
   * next call starts a fresh instance; all state is in files, so nothing is lost.
   */
  call<T = unknown>(op: string, args: unknown = {}): T {
    if (this.busy) throw new CoreError(op, 'the core is busy')
    this.busy = true
    try {
      return this.run<T>(op, args)
    } finally {
      this.busy = false
    }
  }

  private run<T>(op: string, args: unknown): T {
    const wasm = this.instance()
    const request = encoder.encode(JSON.stringify({ op, args: args ?? {} }))
    let reply: string
    try {
      const ptr = wasm.alloc(request.length)
      new Uint8Array(wasm.memory.buffer, ptr, request.length).set(request)
      const packed = wasm.call(ptr, request.length)
      wasm.dealloc(ptr, request.length)
      const out = Number(packed >> 32n)
      const len = Number(packed & 0xffffffffn)
      // Copy before freeing: the view dies with any growth of the memory.
      reply = decoder.decode(new Uint8Array(wasm.memory.buffer, out, len).slice())
      wasm.dealloc(out, len)
    } catch (error) {
      this.exports = null
      throw new CoreError(op, `the core crashed: ${error instanceof Error ? error.message : String(error)}`)
    }
    const parsed = JSON.parse(reply) as { ok?: T; err?: string }
    if (typeof parsed.err === 'string') throw new CoreError(op, parsed.err)
    return parsed.ok as T
  }
}
