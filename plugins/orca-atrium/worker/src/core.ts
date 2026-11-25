// Loads the wasm core (crates/atrium-orca-core) with node:wasi and talks to it in JSON.
//
// ABI: `alloc(len) -> ptr`, `dealloc(ptr, len)`, `call(ptr, len) -> u64` (ptr << 32 | len of the
// reply). A request is `{"op", "args"}`, a reply `{"ok": ...}` or `{"err": "..."}`.
//
// Kept to syntax Node's type stripping accepts (no enums, no parameter properties), so the
// tests can import this file directly.

import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
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

/** The whole file system, seen under its own paths (POSIX), or all drives on Windows. */
export const ROOT: Record<string, string> = process.platform === 'win32' ? winDrives() : { '/': '/' }

function winDrives(): Record<string, string> {
  const out: Record<string, string> = {}
  for (let c = 65; c <= 90; c++) {
    const root = `${String.fromCharCode(c)}:\\`
    if (existsSync(root)) out[`/${String.fromCharCode(c).toLowerCase()}`] = root
  }
  return out
}

function toGuestPath(s: string): string {
  const m = /^([A-Za-z]):(?:[\\/](.*))?$/.exec(s)
  if (!m) return s
  const rest = (m[2] ?? '').replace(/\\/g, '/')
  return rest ? `/${m[1].toLowerCase()}/${rest}` : `/${m[1].toLowerCase()}`
}

function toHostPath(s: string): string {
  const m = /^\/([a-z])(?:\/(.*))?$/.exec(s)
  if (!m) return s
  const rest = (m[2] ?? '').replace(/\//g, '\\')
  return `${m[1].toUpperCase()}:\\${rest}`
}

function mapPaths(v: unknown, fn: (s: string) => string): unknown {
  if (typeof v === 'string') return fn(v)
  if (Array.isArray(v)) return v.map((x) => mapPaths(x, fn))
  if (v && typeof v === 'object') {
    const out: Record<string, unknown> = {}
    for (const [k, val] of Object.entries(v)) out[k] = mapPaths(val, fn)
    return out
  }
  return v
}

function mapping(preopens: Preopens): Record<string, string> {
  const out: Record<string, string> = {}
  if (Array.isArray(preopens)) {
    for (const p of preopens) out[process.platform === 'win32' ? toGuestPath(p) : p] = p
  } else Object.assign(out, preopens)
  for (const guest of Object.keys(out)) if (!existsSync(out[guest]!)) delete out[guest]
  return out
}

/**
 * Node's WASI preview1 on Windows returns ENOSYS (52) for `fd_readdir`.
 * Track open directory fds and serve `fd_readdir` from `fs.readdirSync`.
 */
function patchWinReaddir(
  preview1: Record<string, (...a: unknown[]) => unknown>,
  preopens: Record<string, string>,
  getBuf: () => ArrayBuffer | null
): void {
  const fdPaths = new Map<number, string>()
  const origPrestat = preview1.fd_prestat_dir_name!
  const origOpen = preview1.path_open!
  const origClose = preview1.fd_close!
  const origReaddir = preview1.fd_readdir!

  preview1.fd_prestat_dir_name = (fd: unknown, pathPtr: unknown, pathLen: unknown) => {
    const r = origPrestat(fd, pathPtr, pathLen)
    const mem = getBuf()
    if (r === 0 && mem) {
      const guest = decoder.decode(new Uint8Array(mem, Number(pathPtr), Number(pathLen)).slice()).replace(/\0+$/, '')
      const host = preopens[guest] ?? preopens[`/${guest}`]
      if (host) fdPaths.set(Number(fd), host)
    }
    return r
  }

  preview1.path_open = (...args: unknown[]) => {
    const r = origOpen(...args)
    const mem = getBuf()
    if (r === 0 && mem) {
      const dirfd = Number(args[0])
      const pathPtr = Number(args[2])
      const pathLen = Number(args[3])
      const outFdPtr = Number(args[8])
      const base = fdPaths.get(dirfd)
      if (base) {
        const rel = decoder.decode(new Uint8Array(mem, pathPtr, pathLen).slice())
        const openedFd = new DataView(mem).getUint32(outFdPtr, true)
        fdPaths.set(openedFd, join(base, rel))
      }
    }
    return r
  }

  preview1.fd_close = (fd: unknown) => {
    fdPaths.delete(Number(fd))
    return origClose(fd)
  }

  preview1.fd_readdir = (fd: unknown, bufPtr: unknown, bufLen: unknown, cookie: unknown, bufusedPtr: unknown) => {
    const r = origReaddir(fd, bufPtr, bufLen, cookie, bufusedPtr)
    if (r !== 52) return r
    const hostDir = fdPaths.get(Number(fd))
    const mem = getBuf()
    if (!hostDir || !mem) return r
    let entries
    try {
      entries = readdirSync(hostDir, { withFileTypes: true })
    } catch {
      return 8 // EBADF
    }
    const view = new DataView(mem)
    const bytes = new Uint8Array(mem)
    const start = Number(cookie as bigint)
    const cap = Number(bufLen)
    const base = Number(bufPtr)
    let off = 0
    for (let i = start; i < entries.length; i++) {
      const e = entries[i]!
      const nameBytes = encoder.encode(e.name)
      const recLen = 24 + nameBytes.length
      if (off + recLen > cap) {
        if (off === 0) {
          // Partial header/entry so caller knows buffer was too small.
          const take = Math.min(cap, recLen)
          const tmp = new Uint8Array(recLen)
          const tv = new DataView(tmp.buffer)
          tv.setBigUint64(0, BigInt(i + 1), true)
          tv.setBigUint64(8, BigInt(i + 1), true)
          tv.setUint32(16, nameBytes.length, true)
          tmp[20] = e.isDirectory() ? 3 : e.isFile() ? 4 : 0
          tmp.set(nameBytes, 24)
          bytes.set(tmp.subarray(0, take), base)
          off = take
        }
        break
      }
      view.setBigUint64(base + off, BigInt(i + 1), true)
      view.setBigUint64(base + off + 8, BigInt(i + 1), true)
      view.setUint32(base + off + 16, nameBytes.length, true)
      bytes[base + off + 20] = e.isDirectory() ? 3 : e.isFile() ? 4 : 0
      bytes[base + off + 21] = 0
      bytes[base + off + 22] = 0
      bytes[base + off + 23] = 0
      bytes.set(nameBytes, base + off + 24)
      off += recLen
    }
    view.setUint32(Number(bufusedPtr), off, true)
    return 0
  }
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
    const imports = wasi.getImportObject() as Record<string, Record<string, (...a: unknown[]) => unknown>>
    let exportsRef: Exports | null = null
    if (process.platform === 'win32') patchWinReaddir(imports.wasi_snapshot_preview1!, this.preopens, () => exportsRef?.memory.buffer ?? null)
    const instance = new WA.Instance(this.module, imports)
    exportsRef = instance.exports as unknown as Exports
    wasi.initialize(instance)
    this.exports = exportsRef
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
    const payload = process.platform === 'win32' ? mapPaths(args ?? {}, toGuestPath) : (args ?? {})
    const request = encoder.encode(JSON.stringify({ op, args: payload }))
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
    return (process.platform === 'win32' ? mapPaths(parsed.ok, toHostPath) : parsed.ok) as T
  }
}
