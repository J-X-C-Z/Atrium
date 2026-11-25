// A minimal observable value.
export function signal<T>(initial: T) {
  let value = initial
  const subs = new Set<(v: T) => void>()
  return {
    get: () => value,
    set(v: T) {
      value = v
      subs.forEach((s) => s(v))
    },
    subscribe(fn: (v: T) => void) {
      subs.add(fn)
      return () => {
        subs.delete(fn)
      }
    },
  }
}
