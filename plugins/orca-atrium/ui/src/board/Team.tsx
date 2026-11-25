// Team roster editor: members, notes for the controller, and ordered stand-ins.
import { useEffect, useMemo, useState } from 'preact/hooks'
import type { Member, ProjectView, ValidationError } from '../../../worker/src/protocol'
import { request } from '../orca'
import { t } from '../i18n'
import { Avatar, Button, EmptyState, Field, IconButton, QuotaBar, Select, TextArea, TextInput, act, toast } from '../design/components'
import { Icon } from '../design/icons'
import { cx } from '../util'
import { flatten } from './tree'

const AGENTS = ['claude', 'codex', 'gemini', 'deepseek', 'opencode', 'antigravity']
const EFFORTS = ['low', 'medium', 'high', 'xhigh']

function validate(ms: Member[]): { global: string[]; byIndex: Record<number, string[]> } {
  const byIndex: Record<number, string[]> = {}
  const add = (i: number, m: string) => (byIndex[i] ??= []).push(m)
  const names = new Map<string, number>()
  ms.forEach((m, i) => {
    const n = m.name.trim()
    if (!n) add(i, t('errEmptyName'))
    else if (names.has(n)) add(i, t('errDuplicateName', { name: n }))
    names.set(n, i)
  })
  ms.forEach((m, i) =>
    (m.standIns ?? []).forEach((s) => {
      if (s === m.name) add(i, t('errSelfStandIn', { name: s }))
      else if (!ms.some((x) => x.name === s)) add(i, t('errUnknownStandIn', { name: s }))
    }),
  )
  return { global: [], byIndex }
}

export function Team({ p }: { p: ProjectView }) {
  const [draft, setDraft] = useState<Member[]>(p.team)
  const [sel, setSel] = useState(0)
  const [busy, setBusy] = useState(false)
  const [server, setServer] = useState<ValidationError[]>([])
  const base = useMemo(() => JSON.stringify(p.team), [p.team])
  const dirty = JSON.stringify(draft) !== base

  // Take the worker's roster when nothing is being edited.
  useEffect(() => {
    if (!dirty) setDraft(p.team)
  }, [base])

  const nodes = useMemo(() => flatten(p.org), [p.org])
  const liveOf = (name: string) => nodes.find((n) => n.member === name && n.live === 'working')?.live ?? nodes.find((n) => n.member === name)?.live
  const quotaOf = (name: string) => nodes.find((n) => n.member === name)?.quota
  const v = useMemo(() => validate(draft), [draft])
  const errCount = Object.keys(v.byIndex).length + server.length
  const cur = draft[sel] ?? draft[0]
  const curIdx = draft[sel] ? sel : 0

  const patch = (i: number, f: Partial<Member>) => setDraft((d) => d.map((m, k) => (k === i ? { ...m, ...f } : m)))
  const rename = (i: number, name: string) =>
    setDraft((d) => {
      const old = d[i].name
      return d.map((m, k) => (k === i ? { ...m, name } : { ...m, standIns: m.standIns?.map((s) => (s === old ? name : s)) }))
    })
  const remove = (i: number) => {
    const name = draft[i].name
    setDraft((d) => d.filter((_, k) => k !== i).map((m) => ({ ...m, standIns: m.standIns?.filter((s) => s !== name) })))
    setSel(Math.max(0, i - 1))
  }
  const add = () => {
    setDraft((d) => [...d, { name: `${t('newMember')} ${d.length + 1}`, agent: 'claude', standIns: [] }])
    setSel(draft.length)
  }
  const save = async () => {
    setBusy(true)
    let res: { errors: ValidationError[] } | undefined
    const ok = await act(async () => {
      res = (await request('team.save', { root: p.root, members: draft })) as { errors: ValidationError[] }
    })
    setBusy(false)
    if (!ok) return
    setServer(res?.errors ?? [])
    if (!res?.errors.length) toast(t('teamSaved'), 'ok')
  }

  if (draft.length === 0)
    return (
      <div class="tm-empty">
        <EmptyState icon="users" title={t('teamEmpty')} hint={t('teamEmptyHint')} action={<Button variant="primary" icon="plus" onClick={add}>{t('addMember')}</Button>} />
      </div>
    )

  return (
    <div class="tm">
      <div class="tm-intro">
        <p class="muted">{t('teamIntro')}</p>
      </div>
      <div class="tm-grid">
        <section class="bd-card tm-list" aria-label={t('tabTeam')}>
          <ul>
            {draft.map((m, i) => {
              const q = quotaOf(m.name)
              const errs = v.byIndex[i]
              return (
                <li key={i}>
                  <button type="button" class={cx('tm-row', i === curIdx && 'is-sel', errs && 'has-error')} onClick={() => setSel(i)} aria-current={i === curIdx}>
                    <Avatar size={32} agent={m.agent} state={liveOf(m.name)} badge={q?.limited ? 'limited' : undefined} />
                    <span class="tm-row-main">
                      <span class="tm-row-name ellipsis">{m.name || t('memberName')}</span>
                      <span class="tm-row-sub ellipsis muted">{[m.agent, m.model].filter(Boolean).join(' · ')}</span>
                      {(m.standIns?.length ?? 0) > 0 && (
                        <span class="tm-chain ellipsis">
                          <Icon name="swap" size={11} />
                          {m.standIns!.join(' › ')}
                        </span>
                      )}
                    </span>
                    {errs && <Icon name="alert" size={14} class="tm-err-icon" />}
                    <span class="tm-quota">
                      <QuotaBar used={q?.used} limited={q?.limited} />
                    </span>
                  </button>
                </li>
              )
            })}
          </ul>
          <div class="tm-add">
            <Button variant="secondary" size="sm" icon="plus" onClick={add}>
              {t('addMember')}
            </Button>
          </div>
        </section>

        {cur && (
          <section class="bd-card tm-form" aria-label={cur.name}>
            <div class="tm-fields">
              <Field label={t('memberName')} error={v.byIndex[curIdx]?.[0]}>
                <TextInput value={cur.name} onInput={(x) => rename(curIdx, x)} invalid={!!v.byIndex[curIdx]} />
              </Field>
              <Field label={t('agentKind')}>
                <Select value={cur.agent} onChange={(x) => patch(curIdx, { agent: x })} options={[...(AGENTS.includes(cur.agent) ? [] : [cur.agent]), ...AGENTS].map((a) => ({ value: a, label: a }))} />
              </Field>
              <Field label={t('model')}>
                <TextInput value={cur.model ?? ''} onInput={(x) => patch(curIdx, { model: x || undefined })} mono />
              </Field>
              <Field label={t('effort')}>
                <Select value={cur.effort ?? ''} onChange={(x) => patch(curIdx, { effort: x || undefined })} options={[{ value: '', label: t('effortDefault') }, ...EFFORTS.map((e) => ({ value: e, label: e }))]} />
              </Field>
            </div>
            <Field label={t('folders')}>
              <Chips values={cur.folders ?? []} onChange={(x) => patch(curIdx, { folders: x })} placeholder={t('foldersPlaceholder')} />
            </Field>
            <Field label={t('notes')}>
              <TextArea value={cur.notes ?? ''} onInput={(x) => patch(curIdx, { notes: x || undefined })} rows={3} placeholder={t('notesPlaceholder')} />
            </Field>
            <StandIns all={draft} index={curIdx} onChange={(x) => patch(curIdx, { standIns: x })} />
            <div class="tm-danger">
              <Button size="sm" variant="danger" icon="trash" onClick={() => remove(curIdx)}>
                {t('removeMember')}
              </Button>
            </div>
          </section>
        )}
      </div>

      {(dirty || server.length > 0) && (
        <div class="tm-save" role="region" aria-label={t('unsavedChanges')}>
          <div class="row grow">
            {errCount > 0 ? <Icon name="alert" size={15} class="tm-save-bad" /> : <span class="chip-dot tone-wait" style={{ background: 'var(--s-wait)' }} />}
            <span>{errCount > 0 ? t('cannotSave') : t('unsavedChanges')}</span>
            {server.map((e, i) => (
              <span key={i} class="field-error">{e.message}</span>
            ))}
          </div>
          <Button variant="ghost" onClick={() => { setDraft(p.team); setServer([]) }}>
            {t('discard')}
          </Button>
          <Button variant="primary" icon="check" loading={busy} disabled={errCount > 0 && Object.keys(v.byIndex).length > 0} onClick={save}>
            {t('save')}
          </Button>
        </div>
      )}
    </div>
  )
}

export function Chips({ values, onChange, placeholder }: { values: string[]; onChange: (v: string[]) => void; placeholder: string }) {
  const [text, setText] = useState('')
  const commit = () => {
    const x = text.trim()
    if (x && !values.includes(x)) onChange([...values, x])
    setText('')
  }
  return (
    <div class="chips">
      {values.map((v) => (
        <span key={v} class="chip-edit mono">
          {v}
          <button type="button" aria-label={`${t('remove')} ${v}`} onClick={() => onChange(values.filter((x) => x !== v))}>
            <Icon name="x" size={11} />
          </button>
        </span>
      ))}
      <input
        class="chips-input"
        value={text}
        placeholder={values.length ? '' : placeholder}
        aria-label={placeholder}
        onInput={(e) => setText((e.target as HTMLInputElement).value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault()
            commit()
          } else if (e.key === 'Backspace' && !text && values.length) onChange(values.slice(0, -1))
        }}
        onBlur={commit}
      />
    </div>
  )
}

function StandIns({ all, index, onChange }: { all: Member[]; index: number; onChange: (v: string[]) => void }) {
  const me = all[index]
  const list = me.standIns ?? []
  const [drag, setDrag] = useState<number | null>(null)
  const [over, setOver] = useState<number | null>(null)
  const options = all.filter((m, i) => i !== index && m.name && !list.includes(m.name))
  const swap = (a: number, b: number) => {
    if (b < 0 || b >= list.length || a === b) return
    const n = [...list]
    n.splice(b, 0, n.splice(a, 1)[0])
    onChange(n)
  }
  return (
    <div class="si">
      <div class="field-label">{t('standIns')}</div>
      {list.length === 0 && <div class="muted si-none">{t('noStandIn')}</div>}
      <ol class="si-list">
        {list.map((name, i) => {
          const m = all.find((x) => x.name === name)
          return (
            <li
              key={name}
              class={cx('si-item', drag === i && 'is-drag', over === i && drag !== i && 'is-over')}
              draggable
              onDragStart={(e) => {
                setDrag(i)
                e.dataTransfer?.setData('text/plain', String(i))
              }}
              onDragOver={(e) => {
                e.preventDefault()
                setOver(i)
              }}
              onDrop={(e) => {
                e.preventDefault()
                if (drag != null) swap(drag, i)
                setDrag(null)
                setOver(null)
              }}
              onDragEnd={() => {
                setDrag(null)
                setOver(null)
              }}
            >
              <span class="si-grip" title={t('dragToOrder')}>
                <Icon name="grip" size={14} />
              </span>
              <span class="si-n num">{i + 1}</span>
              {m ? <Avatar size={22} agent={m.agent} /> : <Icon name="alert" size={14} class="tm-err-icon" />}
              <span class="grow ellipsis">{name}</span>
              <span class="muted si-model ellipsis">{m?.model}</span>
              <IconButton size="sm" icon="up" label={t('moveUp')} disabled={i === 0} onClick={() => swap(i, i - 1)} />
              <IconButton size="sm" icon="down" label={t('moveDown')} disabled={i === list.length - 1} onClick={() => swap(i, i + 1)} />
              <IconButton size="sm" icon="x" label={t('remove')} onClick={() => onChange(list.filter((x) => x !== name))} />
            </li>
          )
        })}
      </ol>
      <Select value="" onChange={(x) => x && onChange([...list, x])} label={t('addStandIn')} options={[{ value: '', label: options.length ? `+ ${t('addStandIn')}` : t('noMatchStandIn') }, ...options.map((m) => ({ value: m.name, label: m.name }))]} />
    </div>
  )
}
