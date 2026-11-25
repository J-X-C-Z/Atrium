// Text the worker sends to agents and shows to the person, in Chinese or English.

import type { Lang } from './protocol.js'

export function resolveLang(setting: unknown, env: NodeJS.ProcessEnv = process.env): Lang {
  if (setting === 'zh' || setting === 'en') return setting
  const tag = env.LC_ALL || env.LC_MESSAGES || env.LANG || Intl.DateTimeFormat().resolvedOptions().locale || ''
  return /^zh/i.test(tag) ? 'zh' : 'en'
}

export interface StepFooter {
  dir: string
  step: string
  item?: string
  cli: string
}

export interface RoleFooter {
  root: string
  role: string
  task?: string
  cli: string
}

/** How a step's agent tells Atrium it is done. Appended to every step prompt. */
export function stepFooter(lang: Lang, f: StepFooter): string {
  const where = f.item ? `${f.step} / ${f.item}` : f.step
  if (lang === 'zh') {
    return [
      '',
      '---',
      `【Atrium】这是工作流步骤「${where}」。请在文件夹 ${f.dir} 中工作（先 cd 进去）。`,
      '完成后，必须运行下面的命令报告结果（一段话的总结；较长的报告写进文件再用 -f）：',
      '  atrium report "<总结>"    或    atrium report -f <文件>',
      '如果无法完成，运行 atrium report "BLOCKED: <原因>"。需要问人时运行 atrium ask "<问题>"，然后结束本轮等待回复。',
      `找不到 atrium 命令时，用：${f.cli}`
    ].join('\n')
  }
  return [
    '',
    '---',
    `[Atrium] This is workflow step "${where}". Work in the folder ${f.dir} (cd there first).`,
    'When you are done you MUST report with one of these (a one-paragraph summary; put a long report in a file and use -f):',
    '  atrium report "<summary>"    or    atrium report -f <file>',
    'If you cannot finish, run atrium report "BLOCKED: <why>". To ask the person, run atrium ask "<question>" and end your turn to wait for the answer.',
    `If the atrium command is not found, use: ${f.cli}`
  ].join('\n')
}

/** Appended to every project role kickoff. */
export function roleFooter(lang: Lang, f: RoleFooter): string {
  const task = f.task ?? '<id>'
  if (lang === 'zh') {
    return [
      '',
      '---',
      `【Atrium】你在项目 ${f.root} 中担任 ${f.role}。用这些命令和 Atrium 沟通：`,
      `  atrium task done ${task} "<结果>"       任务完成`,
      `  atrium task blocked ${task} "<原因>"    任务受阻`,
      '  atrium notice "<消息>"                   给项目发通知',
      '  atrium ask "<问题>"                      需要人来回答',
      '  atrium status                            查看你的分工',
      `找不到 atrium 命令时，用：${f.cli}`
    ].join('\n')
  }
  return [
    '',
    '---',
    `[Atrium] You are ${f.role} in the project ${f.root}. Talk to Atrium with these commands:`,
    `  atrium task done ${task} "<result>"      a task is finished`,
    `  atrium task blocked ${task} "<why>"      a task is blocked`,
    '  atrium notice "<text>"                   post a notice to the project',
    '  atrium ask "<question>"                  a question only the person can answer',
    '  atrium status                            what you are assigned',
    `If the atrium command is not found, use: ${f.cli}`
  ].join('\n')
}

export function nudgeText(lang: Lang, kind: 'step' | 'role'): string {
  if (lang === 'zh') {
    return kind === 'step'
      ? '提醒：你还没有报告结果。如果这一步已完成，请运行 atrium report "<总结>"；如果还没完成，请继续。'
      : '提醒：如果当前任务已完成，请运行 atrium task done <id> "<结果>"；如果受阻，运行 atrium task blocked <id> "<原因>"；否则请继续。'
  }
  return kind === 'step'
    ? 'Reminder: you have not reported yet. If this step is done, run atrium report "<summary>"; if not, please carry on.'
    : 'Reminder: if your current task is done, run atrium task done <id> "<result>"; if it is blocked, atrium task blocked <id> "<why>"; otherwise please carry on.'
}

export function standInNote(lang: Lang, member: string, original: string): string {
  return lang === 'zh'
    ? `【Atrium】${original} 的用量已到上限，由你（${member}）作为替补暂时接手。请接着它的工作继续，先看看文件里已有的进展。\n\n`
    : `[Atrium] ${original} hit a usage limit; you (${member}) are standing in. Continue its work; first look at the progress already in the files.\n\n`
}

export function reviewPrompt(lang: Lang, rubric: string, files: string[], dir: string): string {
  const list = files.length ? files.join(', ') : lang === 'zh' ? '（该文件夹中的产出）' : '(the outputs in the folder)'
  if (lang === 'zh') {
    return [
      `请审阅 ${dir} 中的这些文件：${list}。`,
      `标准：${rubric}`,
      '审阅完成后运行 atrium report "PASS: <理由>" 或 atrium report "FAIL: <问题和修改建议>"。第一个词必须是 PASS 或 FAIL。'
    ].join('\n')
  }
  return [
    `Review these files in ${dir}: ${list}.`,
    `Rubric: ${rubric}`,
    'When done, run atrium report "PASS: <why>" or atrium report "FAIL: <problems and how to fix them>". The first word must be PASS or FAIL.'
  ].join('\n')
}

export function answerText(lang: Lang, answer: string): string {
  return lang === 'zh' ? `【Atrium】回复你的问题：${answer}` : `[Atrium] Answer to your question: ${answer}`
}

export function statusText(lang: Lang, n: number, m: number): string {
  return lang === 'zh' ? `${n} 项需要你 · ${m} 个运行中` : `${n} need you · ${m} running`
}

export const T = {
  stalled: (lang: Lang, what: string) => (lang === 'zh' ? `${what} 停下了，没有报告` : `${what} stopped without reporting`),
  blocked: (lang: Lang, what: string) => (lang === 'zh' ? `${what} 需要你批准` : `${what} is waiting for approval`),
  question: (lang: Lang, what: string) => (lang === 'zh' ? `${what} 有问题要问你` : `${what} has a question`),
  gate: (lang: Lang, what: string) => (lang === 'zh' ? `${what} 等你决定` : `${what} needs your decision`),
  quota: (lang: Lang, member: string, standIn?: string) =>
    lang === 'zh'
      ? standIn
        ? `${member} 已达用量上限，替补 ${standIn} 接手`
        : `${member} 已达用量上限，没有可用的替补`
      : standIn
        ? `${member} hit its limit; ${standIn} stands in`
        : `${member} hit its limit; no stand-in is free`,
  error: (lang: Lang, what: string) => (lang === 'zh' ? `${what} 出错了` : `${what} failed`),
  noWorkspace: (lang: Lang) => (lang === 'zh' ? '请先在 Orca 中打开一个工作区。' : 'Open a workspace in Orca first.'),
  openHint: (lang: Lang) =>
    lang === 'zh' ? 'Atrium 在右侧边栏（图层图标）。' : 'Atrium is in the right sidebar (the layers icon).',
  pickTemplate: (lang: Lang) =>
    lang === 'zh' ? '在 Atrium 面板的「工作流」里选择一个模板开始。' : 'Pick a template under Workflows in the Atrium panel.'
}
