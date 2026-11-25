// What `atrium <command>` does: the handler behind the bridge socket.

import type { CliHandler, CliReply, CliRequest } from '../bridge/server.js'
import type { Binding } from '../state.js'
import type { Ctx } from './context.js'
import type { Projects } from './projects.js'
import type { Runs } from './runs.js'

const HELP_EN = [
  'atrium report <text> | -f <file>       report the result of your workflow step',
  'atrium status                          what you are assigned',
  'atrium task done|blocked <id> [note]   mark a project task',
  'atrium notice <text>                   post a notice to the project',
  'atrium ask <question>                  ask the person (a "needs you" item)'
].join('\n')

const HELP_ZH = [
  'atrium report <文本> | -f <文件>        报告工作流步骤的结果',
  'atrium status                          查看分给你的工作',
  'atrium task done|blocked <id> [说明]    标记项目任务',
  'atrium notice <文本>                    给项目发通知',
  'atrium ask <问题>                       向用户提问（进入「需要你」）'
].join('\n')

export function makeCliHandler(ctx: Ctx, runs: Runs, projects: Projects): CliHandler {
  const bindingOf = (r: CliRequest): Binding | undefined => {
    const c = r.context ?? {}
    return (
      (r.launch ? ctx.store.byLaunch(r.launch) : undefined) ??
      (r.terminal ? ctx.store.byTerminal(r.terminal) : undefined) ??
      (c.ATRIUM_RUN && c.ATRIUM_STEP ? ctx.store.forStep(c.ATRIUM_RUN, c.ATRIUM_STEP, c.ATRIUM_ITEM) : undefined) ??
      (c.ATRIUM_PROJECT && c.ATRIUM_ROLE ? ctx.store.forRole(c.ATRIUM_PROJECT, c.ATRIUM_ROLE) : undefined)
    )
  }
  const fail = (error: string): CliReply => ({ ok: false, error })

  return async (request) => {
    const zh = ctx.lang() === 'zh'
    if (request.cmd === 'help') return { ok: true, text: zh ? HELP_ZH : HELP_EN }
    const b = bindingOf(request)
    if (!b) return fail(zh ? '这个终端不是由 Atrium 启动的。' : 'This terminal was not started by Atrium.')
    const text = request.text ?? request.args.join(' ')
    const role = b.kind === 'role'
    switch (request.cmd) {
      case 'report':
        if (!text.trim()) return fail(zh ? '报告内容为空。' : 'The report is empty.')
        if (role) {
          projects.notice(b.root!, text, b.role)
          return { ok: true, text: zh ? '已记入项目通知。' : 'Added to the project notices.' }
        }
        return { ok: true, text: await runs.report(b, text) }
      case 'status':
        if (role) return { ok: true, text: projects.statusText(b) }
        return { ok: true, text: `run ${b.dir}\nstep ${b.step}${b.item ? ` / ${b.item}` : ''}${b.kind === 'review' ? ' (review)' : ''}` }
      case 'task': {
        const [outcome, id, ...note] = request.args
        if (!role) return fail(zh ? 'task 只用于项目角色。' : '`task` is for project roles.')
        if ((outcome !== 'done' && outcome !== 'blocked') || !id) return fail('usage: atrium task done|blocked <id> [note]')
        return { ok: true, text: projects.taskReport(b, id, outcome, request.text ?? note.join(' ')) }
      }
      case 'notice':
        if (!role) return fail(zh ? 'notice 只用于项目角色。' : '`notice` is for project roles.')
        if (!text.trim()) return fail(zh ? '通知内容为空。' : 'The notice is empty.')
        projects.notice(b.root!, text, b.role)
        return { ok: true, text: zh ? '已发布。' : 'Posted.' }
      case 'ask':
        if (!text.trim()) return fail(zh ? '问题为空。' : 'The question is empty.')
        return { ok: true, text: projects.ask(b, text) }
      default:
        return fail(`unknown command ${request.cmd}\n${zh ? HELP_ZH : HELP_EN}`)
    }
  }
}
