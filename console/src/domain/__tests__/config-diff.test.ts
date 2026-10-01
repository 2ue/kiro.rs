import { describe, expect, it } from 'vitest'
import { diff, getAt, merge3, setAt } from '../config-diff'

describe('config-diff', () => {
  it('get/set paths immutably', () => {
    const a = { x: { y: 1, z: [1] } }
    const b = setAt(a, 'x.y', 2)
    expect(getAt(b, 'x.y')).toBe(2)
    expect(a.x.y).toBe(1)
    expect(b.x.z).toBe(a.x.z)
  })

  it('diff lists leaf changes, arrays compared as a whole', () => {
    const changes = diff({ a: 1, b: { c: [1, 2], d: true } }, { a: 1, b: { c: [1, 3], d: false } })
    expect(changes.map((c) => c.path).sort()).toEqual(['b.c', 'b.d'])
  })

  it('merge3 keeps remote changes to fields the user did not touch', () => {
    const base = { a: 1, b: 1, ext: { x: 1 } }
    const remote = { a: 1, b: 1, ext: { x: 9 } } // 另一页面修改了外部池字段
    const draft = { a: 2, b: 1, ext: { x: 1 } } // 用户只改了 a
    const { merged, conflicts } = merge3(base, remote, draft)
    expect(merged).toEqual({ a: 2, b: 1, ext: { x: 9 } })
    expect(conflicts).toEqual([])
  })

  it('merge3 reports conflicts and keeps user value', () => {
    const { merged, conflicts } = merge3({ a: 1 }, { a: 5 }, { a: 2 })
    expect(merged).toEqual({ a: 2 })
    expect(conflicts).toHaveLength(1)
    expect(conflicts[0]).toMatchObject({ path: 'a', before: 5, after: 2 })
  })

  it('merge3 with identical concurrent change is not a conflict', () => {
    expect(merge3({ a: 1 }, { a: 2 }, { a: 2 }).conflicts).toEqual([])
  })
})
