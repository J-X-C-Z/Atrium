// Left-to-right layered layout for a run's step graph.
import type { StepView } from '../../../worker/src/protocol'

export const NODE_W = 160
export const GAP_X = 32
export const GAP_Y = 22
export const PAD = 28
export const PAD_TOP = 84
const BASE_H = 62
const GATE_H = 110
const CELL = 12
const CELL_GAP = 3

export interface Box {
  step: StepView
  layer: number
  x: number
  y: number
  w: number
  h: number
  /** Where edges attach, in canvas coordinates. */
  inX: number
  inY: number
  outX: number
  outY: number
  cols: number
}

export function gridCols() {
  return Math.floor((NODE_W - 24 + CELL_GAP) / (CELL + CELL_GAP))
}

export function nodeHeight(s: StepView): number {
  if (s.kind === 'gate') return GATE_H
  if (s.kind === 'each' && s.items?.length) {
    const rows = Math.ceil(s.items.length / gridCols())
    return BASE_H + 22 + rows * (CELL + CELL_GAP)
  }
  return BASE_H
}

export function layoutSteps(steps: StepView[], dir: 'h' | 'v' = 'h'): { boxes: Map<string, Box>; width: number; height: number } {
  const byId = new Map(steps.map((s) => [s.id, s]))
  const layer = new Map<string, number>()
  const depth = (id: string, seen: Set<string>): number => {
    if (layer.has(id)) return layer.get(id)!
    if (seen.has(id)) return 0
    seen.add(id)
    const s = byId.get(id)
    const ds = (s?.needs ?? []).filter((n) => byId.has(n)).map((n) => depth(n, seen) + 1)
    const d = ds.length ? Math.max(...ds) : 0
    layer.set(id, d)
    return d
  }
  steps.forEach((s) => depth(s.id, new Set()))
  const nLayers = Math.max(0, ...layer.values()) + 1
  const cols: StepView[][] = Array.from({ length: nLayers }, () => [])
  steps.forEach((s) => cols[layer.get(s.id)!].push(s))

  // order inside a layer by the mean position of the parents
  const pos = new Map<string, number>()
  cols[0]?.forEach((s, i) => pos.set(s.id, i))
  for (let l = 1; l < nLayers; l++) {
    cols[l].sort((a, b) => bary(a) - bary(b))
    cols[l].forEach((s, i) => pos.set(s.id, i))
  }
  function bary(s: StepView) {
    const ps = s.needs.filter((n) => pos.has(n)).map((n) => pos.get(n)!)
    return ps.length ? ps.reduce((a, b) => a + b, 0) / ps.length : 0
  }

  const boxes = new Map<string, Box>()
  const make = (s: StepView, x: number, y: number, h: number): Box => {
    const gate = s.kind === 'gate'
    const base = { step: s, layer: layer.get(s.id)!, x, y, w: NODE_W, h, cols: gridCols() }
    if (dir === 'v') return { ...base, inX: x + NODE_W / 2, inY: gate ? y + 6 : y, outX: x + NODE_W / 2, outY: y + h }
    const midY = gate ? y + 30 : y + (s.kind === 'each' ? BASE_H : h) / 2
    return { ...base, inX: gate ? x + NODE_W / 2 - 28 : x, inY: midY, outX: gate ? x + NODE_W / 2 + 28 : x + NODE_W, outY: midY }
  }
  if (dir === 'v') {
    const GV = 40
    const colW = (c: StepView[]) => c.length * NODE_W + Math.max(0, c.length - 1) * GAP_X
    const W = Math.max(0, ...cols.map(colW))
    let y = PAD
    cols.forEach((c) => {
      const lh = Math.max(0, ...c.map(nodeHeight))
      let x = PAD + (W - colW(c)) / 2
      for (const s of c) {
        boxes.set(s.id, make(s, x, y, nodeHeight(s)))
        x += NODE_W + GAP_X
      }
      y += lh + GV
    })
    return { boxes, width: PAD * 2 + W, height: y - GV + PAD }
  }
  const heights = cols.map((c) => c.reduce((a, s) => a + nodeHeight(s), 0) + Math.max(0, c.length - 1) * GAP_Y)
  const H = Math.max(0, ...heights)
  cols.forEach((c, l) => {
    let y = PAD_TOP + (H - heights[l]) / 2
    for (const s of c) {
      const h = nodeHeight(s)
      boxes.set(s.id, make(s, PAD + l * (NODE_W + GAP_X), y, h))
      y += h + GAP_Y
    }
  })
  return { boxes, width: PAD * 2 + nLayers * NODE_W + (nLayers - 1) * GAP_X, height: PAD_TOP + H + PAD }
}

export { CELL, CELL_GAP }
