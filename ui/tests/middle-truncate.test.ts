// Run with: node --test ui/tests/   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { middleTruncate } from '../src/lib/middle-truncate.ts'

// Every character counts as one unit, so `width` is the number of characters that fit.
const fitsIn = (width: number) => (candidate: string) => Array.from(candidate).length <= width

test('text that fits is left alone', () => {
  assert.equal(middleTruncate('r65250409@gmail.com', fitsIn(40)), 'r65250409@gmail.com')
})

test('an email keeps its whole domain and cuts the name', () => {
  assert.equal(middleTruncate('virendrayadav9170643364@gmail.com', fitsIn(24)), 'virendrayad***@gmail.com')
  assert.equal(middleTruncate('KopczynskiLienha.rd850@gmail.com', fitsIn(20)), 'Kopczyn***@gmail.com')
})

test('once fewer than four name characters would remain, the cut is centred', () => {
  // 16 = 3 name chars + *** + @gmail.com: too little of the name, so the domain gives way.
  assert.equal(middleTruncate('virendrayadav9170643364@gmail.com', fitsIn(16)), 'virendr***il.com')
  assert.equal(middleTruncate('virendrayadav9170643364@gmail.com', fitsIn(12)), 'viren***.com')
})

test('a hidden suffix keeps its trailing marker and shortens the name', () => {
  assert.equal(middleTruncate('virendrayadav9170643364***', fitsIn(14)), 'virendrayad***')
})

test('text without an email is cut in the middle', () => {
  assert.equal(middleTruncate('ksk_abcdefghijklmnop', fitsIn(11)), 'ksk_***mnop')
})
