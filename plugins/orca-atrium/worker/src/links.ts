// Link files that open our custom editors: `.atrium/atrium.board` (project) and
// `.atrium/view.atrium-run` (run). Kept out of git through `.atrium/.gitignore`.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import type { Host } from './host.js'
import { HostError } from './host.js'
import { contains, type Workspaces } from './workspaces.js'

export const BOARD_FILE = 'atrium.board'
export const RUN_FILE = 'view.atrium-run'

function ignore(atriumDir: string, name: string): void {
  const path = join(atriumDir, '.gitignore')
  const text = existsSync(path) ? readFileSync(path, 'utf8') : ''
  const lines = text.split(/\r?\n/)
  if (lines.includes(name)) return
  writeFileSync(path, `${text}${text && !text.endsWith('\n') ? '\n' : ''}${name}\n`)
}

function writeLink(base: string, file: string, body: Record<string, string>): string {
  const dir = join(base, '.atrium')
  mkdirSync(dir, { recursive: true })
  const path = join(dir, file)
  const text = `${JSON.stringify(body, null, 2)}\n`
  if (!existsSync(path) || readFileSync(path, 'utf8') !== text) writeFileSync(path, text)
  ignore(dir, file)
  return path
}

export function writeBoardLink(root: string): string {
  return writeLink(root, BOARD_FILE, { project: root })
}

export function writeRunLink(dir: string): string {
  return writeLink(dir, RUN_FILE, { run: dir })
}

/** `link.read`: what a link file the editor got points at. */
export function readLink(text: string): { kind: 'board'; root: string } | { kind: 'run'; dir: string } {
  let value: unknown
  try {
    value = JSON.parse(text)
  } catch {
    throw new HostError('invalid_params', 'not an Atrium link file')
  }
  const v = value as Record<string, unknown>
  if (typeof v.project === 'string' && v.project) return { kind: 'board', root: v.project }
  if (typeof v.run === 'string' && v.run) return { kind: 'run', dir: v.run }
  throw new HostError('invalid_params', 'not an Atrium link file')
}

/** Writes the link and opens it in the workspace that holds it. */
export async function openLink(host: Host, workspaces: Workspaces, path: string, preferred?: string): Promise<void> {
  const first = preferred ? await workspaces.byId(preferred) : undefined
  const ws = first && contains(first.path, path) ? first : await workspaces.forPath(path)
  if (!ws) throw new HostError('action_failed', `no Orca workspace holds ${path}`)
  await host.call('editor.open', { workspaceId: ws.id, path: workspaces.relative(ws, path) })
}
