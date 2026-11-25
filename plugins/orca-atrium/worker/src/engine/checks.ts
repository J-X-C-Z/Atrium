// Checks the core cannot run itself: commands (child_process, with timeouts) and PDF page counts.

import { spawn } from 'node:child_process'
import { readFileSync } from 'node:fs'
import { isAbsolute, relative, resolve } from 'node:path'
import { inflateSync } from 'node:zlib'
import type { CheckSpec } from '../coreapi.js'

/** The core's CheckResult shape. */
export interface CheckResult {
  what: string
  passed: boolean
  detail: string
  log?: string
}

const LOG_CAP = 16 * 1024

export interface CommandOptions {
  command: string
  dir: string
  cwd?: string
  timeoutMs: number
  env?: NodeJS.ProcessEnv
}

export function insideDir(dir: string, path: string): boolean {
  const rel = relative(resolve(dir), resolve(path))
  return rel === '' || (!rel.startsWith('..') && !isAbsolute(rel))
}

/** Runs a shell command in the run folder; passes on exit code 0. */
export function runCommand(opts: CommandOptions): Promise<{ code: number | null; timedOut: boolean; log: string }> {
  const cwd = resolve(opts.dir, opts.cwd ?? '.')
  if (!insideDir(opts.dir, cwd)) {
    return Promise.resolve({ code: null, timedOut: false, log: `cwd ${opts.cwd} is outside the run folder` })
  }
  const win = process.platform === 'win32'
  return new Promise((done) => {
    let log = ''
    const keep = (chunk: Buffer) => {
      log += chunk.toString('utf8')
      if (log.length > LOG_CAP * 2) log = log.slice(-LOG_CAP)
    }
    let child: ReturnType<typeof spawn>
    try {
      child = spawn(win ? process.env.ComSpec || 'cmd.exe' : '/bin/sh', win ? ['/d', '/s', '/c', opts.command] : ['-c', opts.command], {
        cwd,
        env: { ...process.env, ...opts.env },
        detached: !win,
        windowsHide: true,
        stdio: ['ignore', 'pipe', 'pipe']
      })
    } catch (error) {
      done({ code: null, timedOut: false, log: String(error) })
      return
    }
    let timedOut = false
    const timer = setTimeout(() => {
      timedOut = true
      killTree(child)
    }, opts.timeoutMs)
    child.stdout?.on('data', keep)
    child.stderr?.on('data', keep)
    child.on('error', (error) => {
      clearTimeout(timer)
      done({ code: null, timedOut, log: `${log}\n${String(error)}`.slice(-LOG_CAP) })
    })
    child.on('close', (code) => {
      clearTimeout(timer)
      done({ code, timedOut, log: log.slice(-LOG_CAP) })
    })
  })
}

function killTree(child: ReturnType<typeof spawn>): void {
  if (child.pid === undefined) return
  if (process.platform === 'win32') {
    try {
      spawn('taskkill', ['/pid', String(child.pid), '/t', '/f'], { windowsHide: true, stdio: 'ignore' })
    } catch {}
    return
  }
  try {
    process.kill(-child.pid, 'SIGKILL')
  } catch {
    try {
      child.kill('SIGKILL')
    } catch {}
  }
}

/** Page count of a PDF: page objects, also inside compressed object streams; else the largest /Count. */
export function pdfPageCount(bytes: Buffer): number | null {
  if (bytes.subarray(0, 5).toString('latin1') !== '%PDF-') return null
  const texts = [bytes.toString('latin1')]
  // Object streams (PDF 1.5+) hide page objects in Flate streams.
  const raw = texts[0]!
  const streamRe = /stream\r?\n/g
  let m: RegExpExecArray | null
  let scanned = 0
  while ((m = streamRe.exec(raw)) && scanned < 5000) {
    const start = m.index + m[0].length
    const end = raw.indexOf('endstream', start)
    if (end < 0) break
    const header = raw.slice(Math.max(0, m.index - 300), m.index)
    if (/\/ObjStm/.test(header) && /\/FlateDecode/.test(header)) {
      try {
        texts.push(inflateSync(bytes.subarray(start, end)).toString('latin1'))
      } catch {}
    }
    streamRe.lastIndex = end
    scanned++
  }
  let pages = 0
  let maxCount = 0
  for (const text of texts) {
    pages += (text.match(/\/Type\s*\/Page(?![a-zA-Z])/g) ?? []).length
    for (const c of text.matchAll(/\/Type\s*\/Pages\b[^>]*?\/Count\s+(\d+)|\/Count\s+(\d+)[^>]*?\/Type\s*\/Pages\b/g)) {
      maxCount = Math.max(maxCount, Number(c[1] ?? c[2]))
    }
  }
  if (pages > 0) return pages
  if (maxCount > 0) return maxCount
  return null
}

function num(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined
}

export function describeCheck(check: CheckSpec): string {
  if (typeof check.what === 'string' && check.what) return check.what
  switch (check.kind) {
    case 'command':
      return `\`${String(check.run ?? check.command ?? '')}\` succeeds`
    case 'pdf_pages': {
      const path = String(check.path ?? '')
      const min = num(check.min)
      const max = num(check.max)
      if (min !== undefined && max !== undefined) return `${path} has ${min}–${max} pages`
      if (max !== undefined) return `${path} has at most ${max} pages`
      if (min !== undefined) return `${path} has at least ${min} pages`
      return `${path} is a valid PDF`
    }
    default:
      return String(check.kind)
  }
}

/** Runs one command or pdf_pages check. Other kinds are not ours. */
export async function runCheck(dir: string, check: CheckSpec): Promise<CheckResult> {
  const what = describeCheck(check)
  if (check.kind === 'command') {
    const command = String(check.run ?? check.command ?? '')
    if (!command) return { what, passed: false, detail: 'no command' }
    const minutes = num(check.timeout_min) ?? num(check.timeoutMin) ?? 5
    const timeoutMs = num(check.timeout_ms) ?? minutes * 60_000
    const out = await runCommand({ command, dir, ...(typeof check.cwd === 'string' ? { cwd: check.cwd } : {}), timeoutMs })
    const passed = out.code === 0 && !out.timedOut
    const tail = out.log.trim().split('\n').slice(-20).join('\n')
    const message = typeof check.message === 'string' ? check.message : ''
    const detail = passed
      ? ''
      : out.timedOut
        ? `timed out after ${Math.round(timeoutMs / 1000)} s`
        : [message, out.code === null ? 'could not run' : `exit code ${out.code}`, tail].filter(Boolean).join('\n')
    return { what, passed, detail, log: out.log }
  }
  if (check.kind === 'pdf_pages') {
    const path = resolve(dir, String(check.path ?? ''))
    if (!insideDir(dir, path)) return { what, passed: false, detail: 'the path is outside the run folder' }
    let bytes: Buffer
    try {
      bytes = readFileSync(path)
    } catch (error) {
      return { what, passed: false, detail: (error as Error).message }
    }
    const count = pdfPageCount(bytes)
    if (count === null) return { what, passed: false, detail: 'not a PDF, or no page count' }
    const min = num(check.min)
    const max = num(check.max)
    const passed = (max === undefined || count <= max) && (min === undefined || count >= min)
    return { what, passed, detail: passed ? '' : `has ${count} pages` }
  }
  return { what, passed: false, detail: `the worker cannot run a ${check.kind} check` }
}

/** A reviewer's report: first word PASS or FAIL. */
export function readVerdict(report: string): { passed: boolean; detail: string } {
  const text = report.trim()
  const passed = /^(pass|通过)(?![a-z])/i.test(text)
  return { passed, detail: passed ? '' : text.replace(/^(FAIL|不通过)[:：]?\s*/i, '') }
}
