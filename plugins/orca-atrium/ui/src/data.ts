// Fetch-and-follow: load a resource, refetch when the worker posts a matching change.
import { useCallback, useEffect, useRef, useState } from 'preact/hooks'
import type { PostName, Posts } from '../../worker/src/protocol'
import { onPost } from './orca'

export type Resource<T> = { status: 'loading' } | { status: 'error'; error: string } | { status: 'ready'; data: T; refreshing: boolean }

type Watch = { [P in PostName]?: (payload: Posts[P]) => boolean } | PostName[]

export function useResource<T>(load: (() => Promise<T>) | null, deps: unknown[], watch: Watch): [Resource<T>, () => void] {
  const [res, setRes] = useState<Resource<T>>({ status: 'loading' })
  const seq = useRef(0)
  const loadRef = useRef(load)
  loadRef.current = load
  const timer = useRef<number | undefined>(undefined)

  const run = useCallback(async () => {
    const fn = loadRef.current
    if (!fn) return
    const my = ++seq.current
    setRes((r) => (r.status === 'ready' ? { ...r, refreshing: true } : r.status === 'error' ? { status: 'loading' } : r))
    try {
      const data = await fn()
      if (my === seq.current) setRes({ status: 'ready', data, refreshing: false })
    } catch (err) {
      if (my !== seq.current) return
      const msg = (err as Error)?.message ?? String(err)
      // Keep showing the last good data when a refresh fails.
      setRes((r) => (r.status === 'ready' ? { ...r, refreshing: false } : { status: 'error', error: msg }))
    }
  }, [])

  // Coalesce bursts of posts into one refetch.
  const soon = useCallback(() => {
    window.clearTimeout(timer.current)
    timer.current = window.setTimeout(run, 120)
  }, [run])

  useEffect(() => {
    setRes({ status: 'loading' })
    run()
  }, deps)

  useEffect(() => {
    const entries: [PostName, ((p: any) => boolean) | undefined][] = Array.isArray(watch) ? watch.map((n) => [n, undefined]) : (Object.entries(watch) as [PostName, (p: any) => boolean][])
    const subs = entries.map(([name, match]) => onPost(name, (p) => (!match || match(p)) && soon()))
    return () => subs.forEach((s) => s.dispose())
  }, deps)

  return [res, run]
}

/** Re-render every `ms` (for elapsed times). */
export function useTick(ms = 30000) {
  const [, set] = useState(0)
  useEffect(() => {
    const id = window.setInterval(() => set((n) => n + 1), ms)
    return () => window.clearInterval(id)
  }, [ms])
}
