// The template document: text in/out through Orca's custom-editor document API.
import { useCallback, useEffect, useRef, useState } from 'preact/hooks'
import { orcaDocument, type DocumentSession } from '../orca'

export interface Doc {
  status: 'loading' | 'ready' | 'error'
  text: string
  readOnly: boolean
  error?: string
  /** Set by an outside change (an agent, or a merge) so the view can say so. */
  outside: number
  change(next: string): void
}

export function useDoc(): Doc {
  const [text, setText] = useState('')
  const [status, setStatus] = useState<Doc['status']>('loading')
  const [error, setError] = useState<string>()
  const [readOnly, setReadOnly] = useState(false)
  const [outside, setOutside] = useState(0)
  const send = useRef<(v: string) => void>(() => {})

  useEffect(() => {
    const api = orcaDocument()
    if (!api) {
      setStatus('error')
      setError('document API missing')
      return
    }
    let dead = false
    const subs: ({ dispose(): void } | void)[] = []
    const failed = (e: unknown) => {
      if (dead) return
      setStatus('error')
      setError((e as Error)?.message ?? String(e))
    }
    if (api.session) {
      api
        .session<string>({ decode: (c) => (c.kind === 'text' ? c.text : ''), encode: (v) => ({ kind: 'text', text: v }), debounceMs: 250 })
        .then((s: DocumentSession<string>) => {
          if (dead) return
          setText(s.value)
          setReadOnly(s.readOnly)
          setStatus('ready')
          send.current = (v) => s.change(v)
          subs.push(
            s.onReplace((v) => {
              setText(v)
              setOutside((n) => n + 1)
            }),
          )
        }, failed)
    } else {
      // Older Orca: do the sync by hand with a debounce.
      let version: number | string = 0
      let timer: number | undefined
      let pending: string | null = null
      const flush = async () => {
        if (pending == null) return
        const v = pending
        pending = null
        try {
          await api.applyEdit({ baseVersion: version, content: { kind: 'text', text: v } })
        } catch {
          const d = await api.get()
          version = d.version
          if (d.content.kind === 'text') setText(d.content.text)
          setOutside((n) => n + 1)
        }
      }
      api.get().then((d) => {
        if (dead) return
        version = d.version
        setReadOnly(d.readOnly)
        setText(d.content.kind === 'text' ? d.content.text : '')
        setStatus('ready')
      }, failed)
      subs.push(
        api.onDidChange((d) => {
          version = d.version
          if (d.content.kind === 'text') setText(d.content.text)
        }),
      )
      send.current = (v) => {
        pending = v
        window.clearTimeout(timer)
        timer = window.setTimeout(flush, 250)
      }
    }
    return () => {
      dead = true
      subs.forEach((s) => s && s.dispose())
    }
  }, [])

  const change = useCallback((next: string) => {
    setText(next)
    send.current(next)
  }, [])
  return { status, text, readOnly, error, outside, change }
}
