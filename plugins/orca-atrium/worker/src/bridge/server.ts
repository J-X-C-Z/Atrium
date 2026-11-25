// The socket `bin/atrium` talks to: a unix socket (a named pipe on Windows), one JSON object per line.

import { createHash } from 'node:crypto'
import { chmodSync, existsSync, mkdirSync, unlinkSync } from 'node:fs'
import net from 'node:net'
import { tmpdir, userInfo } from 'node:os'
import { join } from 'node:path'

export interface CliRequest {
  token?: string
  cmd: string
  args: string[]
  /** Report text read from a file or stdin by the CLI. */
  text?: string
  /** ATRIUM_LAUNCH of the calling agent. */
  launch?: string
  /** ORCA_TERMINAL_HANDLE of the calling agent. */
  terminal?: string
  /** ATRIUM_RUN, ATRIUM_STEP, ATRIUM_ITEM, ATRIUM_PROJECT, ATRIUM_ROLE as the CLI saw them. */
  context?: Record<string, string>
  cwd?: string
}

export interface CliReply {
  ok: boolean
  text?: string
  error?: string
}

export type CliHandler = (request: CliRequest) => Promise<CliReply>

/** A short, per-user, per-plugin socket path (unix paths must stay under ~104 bytes). */
export function socketPath(seed: string, platform = process.platform): string {
  const user = safeUser()
  const tag = createHash('sha256').update(`${user}:${seed}`).digest('hex').slice(0, 12)
  if (platform === 'win32') return `\\\\.\\pipe\\atrium-orca-${tag}`
  const base = process.env.XDG_RUNTIME_DIR && existsSync(process.env.XDG_RUNTIME_DIR) ? process.env.XDG_RUNTIME_DIR : tmpdir()
  return join(base, `atrium-orca-${tag}.sock`)
}

function safeUser(): string {
  try {
    return userInfo().username
  } catch {
    return 'user'
  }
}

const MAX_LINE = 1024 * 1024

export class BridgeServer {
  private server: net.Server | null = null

  constructor(
    readonly path: string,
    private token: string,
    private handler: CliHandler,
    private log: (message: string) => void = () => {}
  ) {}

  async start(): Promise<void> {
    if (process.platform !== 'win32') {
      try {
        mkdirSync(join(this.path, '..'), { recursive: true })
      } catch {}
      // A socket left by a previous worker blocks listen.
      if (existsSync(this.path)) {
        try {
          unlinkSync(this.path)
        } catch {}
      }
    }
    const server = net.createServer((socket) => this.serve(socket))
    await new Promise<void>((resolve, reject) => {
      server.once('error', reject)
      server.listen(this.path, () => {
        server.off('error', reject)
        resolve()
      })
    })
    server.on('error', (error) => this.log(`bridge: ${String(error)}`))
    if (process.platform !== 'win32') {
      try {
        chmodSync(this.path, 0o600)
      } catch {}
    }
    this.server = server
  }

  async stop(): Promise<void> {
    const server = this.server
    this.server = null
    if (!server) return
    await new Promise<void>((resolve) => server.close(() => resolve()))
    if (process.platform !== 'win32' && existsSync(this.path)) {
      try {
        unlinkSync(this.path)
      } catch {}
    }
  }

  private serve(socket: net.Socket): void {
    let buffer = ''
    socket.setEncoding('utf8')
    socket.on('error', () => {})
    socket.on('data', (chunk: string) => {
      buffer += chunk
      if (buffer.length > MAX_LINE) {
        this.reply(socket, { ok: false, error: 'request too large' })
        socket.end()
        return
      }
      let at: number
      while ((at = buffer.indexOf('\n')) >= 0) {
        const line = buffer.slice(0, at)
        buffer = buffer.slice(at + 1)
        if (line.trim()) void this.answer(socket, line)
      }
    })
  }

  private async answer(socket: net.Socket, line: string): Promise<void> {
    let request: CliRequest
    try {
      request = JSON.parse(line) as CliRequest
    } catch {
      this.reply(socket, { ok: false, error: 'not JSON' })
      return
    }
    if (request.token !== this.token) {
      this.reply(socket, { ok: false, error: 'this terminal was not started by Atrium (bad token)' })
      return
    }
    if (typeof request.cmd !== 'string' || !Array.isArray(request.args)) {
      this.reply(socket, { ok: false, error: 'bad request' })
      return
    }
    try {
      this.reply(socket, await this.handler(request))
    } catch (error) {
      this.reply(socket, { ok: false, error: error instanceof Error ? error.message : String(error) })
    }
  }

  private reply(socket: net.Socket, reply: CliReply): void {
    if (!socket.destroyed) socket.write(`${JSON.stringify(reply)}\n`)
  }
}
