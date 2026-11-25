// Notices and handoffs, newest first, with a composer on top.
import { useState } from 'preact/hooks'
import type { ProjectView } from '../../../worker/src/protocol'
import { request } from '../orca'
import { ago, t } from '../i18n'
import { Avatar, Button, EmptyState, TextArea, act } from '../design/components'
import { findNode } from './tree'
import { roleName } from '../util'

export function Activity({ p }: { p: ProjectView }) {
  const [open, setOpen] = useState(false)
  const [text, setText] = useState('')
  const [busy, setBusy] = useState(false)
  const post = async () => {
    if (!text.trim()) return
    setBusy(true)
    const ok = await act(() => request('project.notice', { root: p.root, text: text.trim() }), t('noticePosted'))
    setBusy(false)
    if (ok) {
      setText('')
      setOpen(false)
    }
  }
  const notices = [...p.notices].sort((a, b) => b.at - a.at)
  return (
    <aside class="bd-card bd-act" aria-labelledby="h-act">
      <div class="bd-card-head">
        <h2 id="h-act" class="bd-card-title">
          {t('activity')}
        </h2>
        <span class="grow" />
        {!open && (
          <Button size="sm" variant="secondary" icon="megaphone" onClick={() => setOpen(true)}>
            {t('addNotice')}
          </Button>
        )}
      </div>
      {open && (
        <form
          class="act-compose"
          onSubmit={(e) => {
            e.preventDefault()
            post()
          }}
        >
          <TextArea value={text} onInput={setText} rows={3} placeholder={t('noticePlaceholder')} aria-label={t('addNotice')} autoFocus onKeyDown={(e) => { if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) post() }} />
          <div class="row" style={{ justifyContent: 'flex-end' }}>
            <Button size="sm" variant="ghost" onClick={() => setOpen(false)}>
              {t('cancel')}
            </Button>
            <Button size="sm" variant="primary" loading={busy} disabled={!text.trim()} {...{ type: 'submit' }}>
              {t('post')}
            </Button>
          </div>
        </form>
      )}
      {notices.length === 0 ? (
        <EmptyState compact icon="message" title={t('activity')} hint={t('noActivity')} />
      ) : (
        <ol class="act-list">
          {notices.map((n, i) => {
            const node = n.from ? findNode(p.org, n.from) : undefined
            return (
              <li key={i} class="act-item">
                <span class="act-who">{node ? <Avatar size={24} agent={node.agent} state={node.live} label={roleName(n.from!)} /> : <span class="act-all"><svg width="14" height="14" viewBox="0 0 16 16"><path d="M2.5 6.5v3h2l5 3v-9l-5 3Z M12 5.8a3 3 0 0 1 0 4.4" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" /></svg></span>}</span>
                <div class="act-body">
                  <div class="act-meta">
                    <b>{n.from ? roleName(n.from) : t('addNotice')}</b>
                    <span class="faint">{ago(n.at)}</span>
                  </div>
                  <p class="act-text">{n.text}</p>
                </div>
              </li>
            )
          })}
        </ol>
      )}
    </aside>
  )
}
