// The task board: columns by status, optional milestone lanes, drag to move.
import { useEffect, useMemo, useState } from 'preact/hooks'
import type { ProjectView, Task, TaskStatus } from '../../../worker/src/protocol'
import { request } from '../orca'
import { t, taskStatusLabel } from '../i18n'
import { Avatar, Button, Menu, Tag, TextInput, Toggle, act, toast, type Tone } from '../design/components'
import { Icon } from '../design/icons'
import { cx, roleName } from '../util'
import { findNode } from './tree'

const COLUMNS: { id: TaskStatus; tone: Tone }[] = [
  { id: 'todo', tone: 'idle' },
  { id: 'ready', tone: 'neutral' },
  { id: 'in_progress', tone: 'run' },
  { id: 'review', tone: 'wait' },
  { id: 'blocked', tone: 'bad' },
  { id: 'done', tone: 'ok' },
]

export function Kanban({ p }: { p: ProjectView }) {
  const [lanes, setLanes] = useState(false)
  const [title, setTitle] = useState('')
  const [over, setOver] = useState<string | null>(null)
  const [dragging, setDragging] = useState<string | null>(null)
  const [moved, setMoved] = useState<Record<string, TaskStatus>>({})
  const [folded, setFolded] = useState<Record<string, boolean>>({})

  // The worker's data wins once it arrives.
  useEffect(() => setMoved({}), [p.tasks])
  const tasks = useMemo(() => p.tasks.map((x) => (moved[x.id] ? { ...x, status: moved[x.id] } : x)), [p.tasks, moved])

  const move = async (id: string, status: TaskStatus) => {
    const cur = tasks.find((x) => x.id === id)
    if (!cur || cur.status === status) return
    setMoved((m) => ({ ...m, [id]: status }))
    const ok = await act(() => request('project.task.move', { root: p.root, id, status }), t('moved', { status: taskStatusLabel(status) }))
    if (!ok)
      setMoved((m) => {
        const n = { ...m }
        delete n[id]
        return n
      })
  }
  const create = async () => {
    const v = title.trim()
    if (!v) return
    setTitle('')
    await act(() => request('project.task.create', { root: p.root, title: v }))
  }

  const columnsFor = (list: Task[], laneId: string) => (
    <div class="kb-cols">
      {COLUMNS.map((c) => {
        const items = list.filter((x) => x.status === c.id)
        const key = `${laneId}/${c.id}`
        return (
          <div
            key={c.id}
            class={cx('kb-col', over === key && 'is-over')}
            onDragOver={(e) => {
              if (!dragging) return
              e.preventDefault()
              setOver(key)
            }}
            onDragLeave={() => setOver((o) => (o === key ? null : o))}
            onDrop={(e) => {
              e.preventDefault()
              setOver(null)
              const id = e.dataTransfer?.getData('text/plain') || dragging
              setDragging(null)
              if (id) move(id, c.id)
            }}
          >
            {items.map((x) => (
              <TaskCard key={x.id} task={x} p={p} onMove={move} dragging={dragging === x.id} onDragStart={() => setDragging(x.id)} onDragEnd={() => { setDragging(null); setOver(null) }} />
            ))}
            {items.length === 0 && <div class="kb-empty">{dragging ? t('dropHere') : t('emptyColumn')}</div>}
          </div>
        )
      })}
    </div>
  )

  const header = (
    <div class="kb-heads">
      {COLUMNS.map((c) => (
        <div key={c.id} class={cx('kb-head', `tone-${c.tone}`)}>
          <span class="kb-dot" />
          <span class="kb-head-title">{taskStatusLabel(c.id)}</span>
          <span class="num faint">{tasks.filter((x) => x.status === c.id).length}</span>
        </div>
      ))}
    </div>
  )

  const laneList = lanes
    ? [...p.milestones.map((m) => ({ id: m.id, title: m.title })), { id: '', title: t('noMilestone') }].map((l) => ({ ...l, list: tasks.filter((x) => (x.milestone ?? '') === l.id) })).filter((l) => l.list.length)
    : []

  return (
    <div class="kb">
      <div class="kb-bar">
        <form
          class="kb-new"
          onSubmit={(e) => {
            e.preventDefault()
            create()
          }}
        >
          <Icon name="plus" size={14} />
          <TextInput value={title} onInput={setTitle} placeholder={t('newTaskPlaceholder')} aria-label={t('newTask')} class="kb-new-input" />
          <Button size="sm" variant="primary" disabled={!title.trim()} {...{ type: 'submit' }}>
            {t('add')}
          </Button>
        </form>
        <span class="grow" />
        <label class="row kb-lanes">
          <Toggle size="sm" checked={lanes} onChange={setLanes} label={t('swimlanes')} />
          <span>{t('swimlanes')}</span>
        </label>
      </div>
      <div class="kb-scroll">
        <div class="kb-grid">
          {header}
          {!lanes && columnsFor(tasks, 'all')}
          {lanes &&
            laneList.map((l) => {
              const done = l.list.filter((x) => x.status === 'done').length
              const fold = folded[l.id] ?? (done === l.list.length)
              return (
                <section key={l.id} class="kb-lane">
                  <button type="button" class="kb-lane-head" aria-expanded={!fold} onClick={() => setFolded({ ...folded, [l.id]: !fold })}>
                    <Icon name={fold ? 'chevronRight' : 'chevronDown'} size={14} />
                    <span class="kb-lane-title">{l.title}</span>
                    <span class="faint num">{done}/{l.list.length}</span>
                  </button>
                  {!fold && columnsFor(l.list, l.id)}
                </section>
              )
            })}
        </div>
      </div>
    </div>
  )
}

function TaskCard({ task, p, onMove, dragging, onDragStart, onDragEnd }: { task: Task; p: ProjectView; onMove: (id: string, s: TaskStatus) => void; dragging: boolean; onDragStart: () => void; onDragEnd: () => void }) {
  const owner = task.owner ? findNode(p.org, task.owner) : undefined
  const ms = p.milestones.find((m) => m.id === task.milestone)
  return (
    <article
      class={cx('task', dragging && 'is-dragging', task.status === 'blocked' && 'is-blocked', task.status === 'done' && 'is-done')}
      draggable
      onDragStart={(e) => {
        e.dataTransfer?.setData('text/plain', task.id)
        if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move'
        onDragStart()
      }}
      onDragEnd={onDragEnd}
    >
      <div class="task-top">
        <span class="mono faint task-id">{task.id}</span>
        {task.pool && <Tag title={t('poolHint')}>{t('pool')}</Tag>}
        <span class="grow" />
        <Menu
          label={t('moveTo')}
          heading={t('moveTo')}
          align="end"
          items={COLUMNS.map((c) => ({ id: c.id, label: taskStatusLabel(c.id), disabled: c.id === task.status }))}
          onSelect={(id) => onMove(task.id, id as TaskStatus)}
          trigger={({ toggle, open, id }) => (
            <button type="button" class="task-menu" onClick={toggle} aria-label={t('moveTo')} aria-haspopup="menu" aria-expanded={open} aria-controls={id} title={t('moveTo')}>
              <Icon name="swap" size={13} />
            </button>
          )}
        />
      </div>
      <h3 class="task-title">{task.title}</h3>
      <div class="task-meta">
        {owner ? (
          <span class="task-owner" title={roleName(task.owner!)}>
            <Avatar size={20} agent={owner.agent} state={owner.live} label={roleName(owner.id)} />
            <span class="ellipsis">{roleName(owner.id)}</span>
          </span>
        ) : (
          <span class="faint task-owner">{t('unassigned')}</span>
        )}
        <span class="grow" />
        {ms && <Tag title={ms.title}>{ms.title}</Tag>}
      </div>
      {task.file && (
        <button
          type="button"
          class="task-file mono ellipsis"
          title={t('taskFile')}
          onClick={() => {
            const text = task.file!
            try {
              navigator.clipboard?.writeText(text)
            } catch {
              /* clipboard is blocked in frames */
            }
            toast(text)
          }}
        >
          <Icon name="file" size={11} />
          <span class="ellipsis">{task.file}</span>
        </button>
      )}
    </article>
  )
}
