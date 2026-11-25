// The org chart: controller → leads → workers, with live rings, quota bars and a details drawer.
import { useMemo, useState } from 'preact/hooks'
import type { ProjectView, RoleNode } from '../../../worker/src/protocol'
import { request } from '../orca'
import { agentStateLabel, duration, t, taskStatusLabel } from '../i18n'
import { Avatar, Button, Chip, Drawer, EmptyState, QuotaBar, StateDot, act, agentTone, toast } from '../design/components'
import { Icon } from '../design/icons'
import { cx, roleKind, roleName, roleTitle } from '../util'
import { findNode, layoutTree } from './tree'

const W = 212
const H = 124
const GX = 22
const GY = 48

export function OrgChart({ p }: { p: ProjectView }) {
  const [sel, setSel] = useState<string | null>(null)
  const lay = useMemo(() => layoutTree(p.org, W, H, GX, GY), [p.org])
  const selected = sel ? findNode(p.org, sel) : undefined
  const empty = p.org.children.length === 0 && !p.org.member && !p.org.live

  return (
    <section class="bd-card bd-org" aria-labelledby="h-org">
      <div class="bd-card-head">
        <h2 id="h-org" class="bd-card-title">
          {t('orgChart')}
        </h2>
        <Legend />
      </div>
      {empty ? (
        <EmptyState icon="users" title={t('orgChart')} hint={t('orgEmpty')} />
      ) : (
        <div class="org-scroll">
          <div class="org-canvas" style={{ width: `${lay.width}px`, height: `${lay.height}px` }}>
            <svg class="org-edges" width={lay.width} height={lay.height} aria-hidden="true">
              {lay.placed
                .filter((n) => n.parent)
                .map((n) => {
                  const a = n.parent!
                  const x1 = a.x + W / 2
                  const y1 = a.y + H
                  const x2 = n.x + W / 2
                  const y2 = n.y
                  const mid = (y1 + y2) / 2
                  const live = a.node.live === 'working' && n.node.live === 'working'
                  return <path key={n.node.id} class={cx('org-edge', live && 'is-live')} d={`M${x1} ${y1} C${x1} ${mid} ${x2} ${mid} ${x2} ${y2}`} />
                })}
            </svg>
            {lay.placed.map((n) => (
              <OrgNode key={n.node.id} node={n.node} x={n.x} y={n.y} selected={sel === n.node.id} onSelect={() => setSel(sel === n.node.id ? null : n.node.id)} />
            ))}
          </div>
        </div>
      )}
      <Drawer
        open={!!selected}
        onClose={() => setSel(null)}
        width={380}
        title={selected ? roleTitle(selected.id) : ''}
        subtitle={selected && <Chip tone={agentTone(selected.live)}>{agentStateLabel(selected.live)}</Chip>}
        actions={selected && <RoleActions p={p} node={selected} />}
      >
        {selected && <RoleDetails p={p} node={selected} />}
      </Drawer>
    </section>
  )
}

function Legend() {
  const items: { s: RoleNode['live']; label: string }[] = [
    { s: 'working', label: t('st_working') },
    { s: 'waiting', label: t('st_waiting') },
    { s: 'blocked', label: t('st_blocked') },
    { s: 'idle', label: t('st_idle') },
  ]
  return (
    <ul class="org-legend" aria-label={t('legend')}>
      {items.map((i) => (
        <li key={i.label}>
          <StateDot state={i.s} size={8} />
          {i.label}
        </li>
      ))}
    </ul>
  )
}

function OrgNode({ node, x, y, selected, onSelect }: { node: RoleNode; x: number; y: number; selected: boolean; onSelect: () => void }) {
  const limited = !!node.quota?.limited
  return (
    <button
      type="button"
      class={cx('org-node', `kind-${roleKind(node.id)}`, selected && 'is-selected', node.live && `live-${node.live}`, `tone-${agentTone(node.live)}`)}
      style={{ left: `${x}px`, top: `${y}px`, width: `${W}px`, height: `${H}px` }}
      onClick={onSelect}
      aria-pressed={selected}
      aria-label={`${roleTitle(node.id)}, ${agentStateLabel(node.live)}`}
    >
      <span class="org-head">
        <Avatar size={36} agent={node.agent} state={node.live} badge={node.standingInFor ? 'standin' : limited ? 'limited' : undefined} label={`${node.agent ?? ''} · ${agentStateLabel(node.live)}`} />
        <span class="org-names">
          <span class="org-role ellipsis">{roleName(node.id)}</span>
          <span class="org-member ellipsis">{[node.member, node.model].filter(Boolean).join(' · ') || t('unassigned')}</span>
        </span>
      </span>
      <span class={cx('org-task', !node.currentTask && 'is-empty')}>{node.currentTask ?? t('noTask')}</span>
      <span class="org-foot">
        {node.standingInFor ? (
          <span class="org-standin">
            <Icon name="swap" size={11} />
            {t('standingInFor', { member: node.standingInFor })}
          </span>
        ) : (
          <span class="org-kind faint">{t(roleKind(node.id))}</span>
        )}
        <span class="grow" />
        <span class="org-quota">
          <QuotaBar used={node.quota?.used} limited={limited} />
        </span>
      </span>
    </button>
  )
}

function RoleDetails({ p, node }: { p: ProjectView; node: RoleNode }) {
  const tasks = p.tasks.filter((x) => x.owner === node.id)
  const q = node.quota
  return (
    <>
      <div class="rd-hero">
        <Avatar size={44} agent={node.agent} state={node.live} badge={node.standingInFor ? 'standin' : undefined} />
        <div class="col" style={{ gap: '2px' }}>
          <div class="rd-name ellipsis">{node.member ?? t('unassigned')}</div>
          <div class="muted rd-sub ellipsis">{[node.agent, node.model].filter(Boolean).join(' · ')}</div>
        </div>
      </div>
      {node.standingInFor && (
        <div class="rd-note tone-wait">
          <Icon name="swap" size={14} />
          <span>{t('coveringNow', { member: node.standingInFor })}</span>
        </div>
      )}
      <dl class="rd-kv">
        <dt>{t('currentTask')}</dt>
        <dd>{node.currentTask ?? <span class="muted">{t('noTask')}</span>}</dd>
        {q && (
          <>
            <dt>{t('quota')}</dt>
            <dd class="col" style={{ gap: '5px' }}>
              <QuotaBar used={q.used} limited={q.limited} />
              <span class="muted rd-quota-text num">{q.limited ? t('quotaLimited') : t('quotaUsed', { p: Math.round((q.used ?? 0) * 100) })}{q.resetsAt ? ` · ${t('resetsIn', { t: duration(Math.max(0, q.resetsAt - Date.now())) })}` : ''}</span>
            </dd>
          </>
        )}
      </dl>
      <section class="col" style={{ gap: '8px' }}>
        <h3 class="rd-h">{t('tasksOwned')}</h3>
        {tasks.length === 0 ? (
          <p class="muted">{t('noOwnedTasks')}</p>
        ) : (
          <ul class="rd-tasks">
            {tasks.map((x) => (
              <li key={x.id}>
                <Chip tone={x.status === 'done' ? 'ok' : x.status === 'blocked' ? 'bad' : x.status === 'in_progress' ? 'run' : x.status === 'review' ? 'wait' : 'idle'}>{taskStatusLabel(x.status)}</Chip>
                <span class="grow">{x.title}</span>
              </li>
            ))}
          </ul>
        )}
      </section>
    </>
  )
}

function RoleActions({ p, node }: { p: ProjectView; node: RoleNode }) {
  const [busy, setBusy] = useState(false)
  const idle = !node.live || node.live === 'idle' || node.live === 'done'
  const toggle = async () => {
    setBusy(true)
    const ok = await act(() => request(idle ? 'role.start' : 'role.stop', { root: p.root, role: node.id }), idle ? t('roleStarted', { role: roleName(node.id) }) : t('roleStopped', { role: roleName(node.id) }))
    setBusy(false)
    if (!ok) toast(t('retry'))
  }
  return (
    <>
      {node.terminalId && (
        <Button icon="terminal" variant="secondary" onClick={() => act(() => request('agent.focus', { terminalId: node.terminalId! }))}>
          {t('focusTerminal')}
        </Button>
      )}
      <span class="grow" />
      <Button icon={idle ? 'play' : 'stop'} variant={idle ? 'primary' : 'danger'} loading={busy} onClick={toggle} title={idle ? undefined : t('stopConfirm')}>
        {idle ? t('startRole') : t('stopRole')}
      </Button>
    </>
  )
}
