// Small helpers shared by the views.
import { t } from './i18n'

export const cx = (...parts: (string | false | null | undefined)[]) => parts.filter(Boolean).join(' ')

export const basename = (p: string) => p.replace(/[\\/]+$/, '').split(/[\\/]/).pop() || p

/** "lead:建模" → "建模", "worker:建模/求解" → "求解", "controller" → "主控". */
export function roleName(id: string): string {
  if (id === 'controller') return t('controller')
  const rest = id.replace(/^(lead|worker):/, '')
  return rest.split('/').pop() ?? rest
}

export function roleKind(id: string): 'controller' | 'lead' | 'worker' {
  return id.startsWith('lead:') ? 'lead' : id.startsWith('worker:') ? 'worker' : 'controller'
}

/** Role with its kind, e.g. "负责人 · 建模". */
export function roleTitle(id: string): string {
  if (id === 'controller') return t('controller')
  return t(roleKind(id) === 'lead' ? 'leadOf' : 'workerOf', { name: roleName(id) })
}

export function sumBy<T>(xs: T[], f: (x: T) => number) {
  return xs.reduce((a, x) => a + f(x), 0)
}

export function clamp(n: number, lo: number, hi: number) {
  return Math.max(lo, Math.min(hi, n))
}
