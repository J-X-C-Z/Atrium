import type { RoleNode } from '../../../worker/src/protocol'

export function findNode(n: RoleNode, id: string): RoleNode | undefined {
  if (n.id === id) return n
  for (const c of n.children) {
    const f = findNode(c, id)
    if (f) return f
  }
  return undefined
}

export function flatten(n: RoleNode, out: RoleNode[] = []): RoleNode[] {
  out.push(n)
  n.children.forEach((c) => flatten(c, out))
  return out
}

export interface Placed {
  node: RoleNode
  x: number
  y: number
  parent?: Placed
}

/** Tidy tree: leaves side by side, parents centred over their children. */
export function layoutTree(root: RoleNode, w: number, h: number, gapX: number, gapY: number): { placed: Placed[]; width: number; height: number } {
  const placed: Placed[] = []
  let cursor = 0
  let maxDepth = 0
  const walk = (n: RoleNode, depth: number, parent?: Placed): Placed => {
    maxDepth = Math.max(maxDepth, depth)
    const me: Placed = { node: n, x: 0, y: depth * (h + gapY), parent }
    placed.push(me)
    if (n.children.length === 0) {
      me.x = cursor
      cursor += w + gapX
    } else {
      const kids = n.children.map((c) => walk(c, depth + 1, me))
      me.x = (kids[0].x + kids[kids.length - 1].x) / 2
    }
    return me
  }
  walk(root, 0)
  return { placed, width: Math.max(w, cursor - gapX), height: (maxDepth + 1) * h + maxDepth * gapY }
}
