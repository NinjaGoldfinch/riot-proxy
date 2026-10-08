// The showcase in a real browser (headless Chromium through playwright-core), at a
// desktop, a tablet and a phone width. jsdom has no layout, so this is where the
// page's geometry is checked: every icon at its CSS size whatever the image's own
// size, nothing wider than its card or the window, compact lines on one line, and
// the parts of a match card side by side without overlapping.
//
// Images are served as real PNGs at Data Dragon's own sizes (champion 120 px, item
// and spell 64 px, profile icon 128 px; rune icons at 256 px, bigger than any box): the icon bug this suite was written for
// only shows once an image loads, and a 404 falls back to a fixed-size box.
//
// Screenshots of every view land in tests/dom/screenshots/ (git-ignored) for review.
//   cd tests/dom && npm ci && npx playwright-core install chromium-headless-shell && npm test
// Without a browser the suite skips, except under CI, where it fails.
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync } from 'node:fs';
import { deflateSync } from 'node:zlib';
import { chromium } from 'playwright-core';

import { html, MATCH, api } from './fake-api.mjs';

const WIDTHS = [['desktop', 1280], ['tablet', 820], ['phone', 390]];
const VIEWS = [
  ['home', '', '#ladder tbody tr td.name small'],
  ['player', '#/player/Faker/KR1', '#pMatches .match img.icon'],
  ['match', `#/match/asia/${MATCH.metadata.matchId}`, '#goldChart svg'],
  ['champion', '#/champion/1', '#cMatchups tbody tr'],
];
const OPTS = { live: true };
// Each icon class and the size it must render at, in CSS pixels.
const ICONS = [['.icon.xs', 22], ['.icon.sm', 28], ['.icon:not(.sm):not(.xs)', 48], ['.avatar', 64]];

// ---------- a PNG of one colour, without a dependency
const crcTable = Array.from({ length: 256 }, (_, n) => { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; return c >>> 0; });
const crc = (buf) => { let c = 0xffffffff; for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8); return (c ^ 0xffffffff) >>> 0; };
function chunk(type, data) {
  const out = Buffer.alloc(12 + data.length);
  out.writeUInt32BE(data.length, 0);
  out.write(type, 4, 'ascii');
  data.copy(out, 8);
  out.writeUInt32BE(crc(out.subarray(4, 8 + data.length)), 8 + data.length);
  return out;
}
function png(size, [r, g, b]) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0); ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; ihdr[9] = 2; // 8-bit RGB
  const row = Buffer.alloc(1 + size * 3);
  for (let x = 0; x < size; x++) row.set([r, g, b], 1 + x * 3);
  const raw = Buffer.concat(Array.from({ length: size }, () => row));
  return Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk('IHDR', ihdr), chunk('IDAT', deflateSync(raw)), chunk('IEND', Buffer.alloc(0))]);
}
const IMAGES = { champion: png(120, [180, 120, 220]), item: png(64, [210, 170, 60]), spell: png(64, [60, 170, 210]), profileicon: png(128, [200, 90, 60]), 'perk-images': png(256, [90, 200, 120]) };

// ---------- browser
let browser = null;
let skip = false;
before(async () => {
  try {
    browser = await chromium.launch(process.env.CHROMIUM_PATH ? { executablePath: process.env.CHROMIUM_PATH } : {});
  } catch (e) {
    if (process.env.CI) throw e;
    skip = `no headless Chromium (${String(e.message).split('\n')[0]}); run: npx playwright-core install chromium-headless-shell`;
  }
  mkdirSync(new URL('./screenshots/', import.meta.url), { recursive: true });
});
after(async () => { await browser?.close(); });

async function open(width, hash, ready) {
  const page = await browser.newPage({ viewport: { width, height: 900 } });
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  await page.addInitScript(() => localStorage.setItem('rp.showcase.platform', '"oc1"'));
  await page.route('http://showcase.test/**', (r) => {
    const u = new URL(r.request().url());
    if (u.pathname === '/dev/showcase') return r.fulfill({ contentType: 'text/html', body: html });
    const img = u.pathname.match(/^\/ddragon\/[^/]+\/img\/([a-z-]+)\//);
    if (img) return IMAGES[img[1]] ? r.fulfill({ contentType: 'image/png', body: IMAGES[img[1]] }) : r.fulfill({ status: 404, body: '' });
    const [status, body] = api([], u.pathname + u.search, OPTS);
    return r.fulfill({ status, contentType: 'application/json', headers: { 'x-cache': 'HIT' }, body: JSON.stringify(body) });
  });
  await page.goto(`http://showcase.test/dev/showcase${hash}`);
  await page.waitForSelector(ready);
  await page.waitForLoadState('networkidle');
  // Every image loaded, so its natural size could push the layout around. Lazy images
  // below the fold would never load in a fixed viewport, so they are made eager.
  await page.evaluate(() => Promise.race([
    Promise.all([...document.images].map((i) => { i.loading = 'eager'; return i.complete ? null : new Promise((r) => { i.addEventListener('load', r); i.addEventListener('error', r); }); })),
    new Promise((r) => setTimeout(r, 5000)),
  ]));
  await page.waitForLoadState('networkidle');
  return { page, errors };
}

/** Everything the checks need, measured in the page in one go. */
function measure({ icons }) {
  const rect = (el) => el.getBoundingClientRect();
  const label = (el) => `${el.tagName.toLowerCase()}${el.id ? `#${el.id}` : ''}.${[...el.classList].join('.')}${el.closest('[id]') ? ` in #${el.closest('[id]').id}` : ''}`;
  const out = { icons: [], overflow: [], wrapped: [], overlaps: [], cells: [], images: [], page: { scroll: document.documentElement.scrollWidth, width: innerWidth } };
  for (const [sel, size] of icons) {
    for (const el of document.querySelectorAll(sel)) {
      const r = rect(el);
      if (!r.width && !r.height) continue; // not displayed at this width
      if (Math.abs(r.width - size) > 0.5 || Math.abs(r.height - size) > 0.5) out.icons.push(`${label(el)}: ${r.width.toFixed(1)}×${r.height.toFixed(1)}, want ${size}`);
    }
  }
  // Content wider than its box (scroll containers are meant to scroll, so they are the boundary).
  for (const el of document.querySelectorAll('.card, .match, .rank, .banner, .who, .card > header, .team-card header')) {
    if (el.closest('.scroll') || !rect(el).width) continue;
    if (el.scrollWidth > el.clientWidth + 1) out.overflow.push(`${label(el)}: ${el.scrollWidth} > ${el.clientWidth}`);
  }
  // Short lines that must not wrap: count the distinct lines its text sits on.
  const textLines = (el) => {
    const tops = [];
    const walk = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
    for (let n = walk.nextNode(); n; n = walk.nextNode()) {
      if (!n.textContent.trim()) continue;
      const range = document.createRange();
      range.selectNodeContents(n);
      for (const r of range.getClientRects()) if (r.width && !tops.some((t) => Math.abs(t - r.bottom) < 4)) tops.push(r.bottom);
    }
    return tops.length;
  };
  for (const el of document.querySelectorAll('.match .meta > *, .match .kda > *, .match .cs > *, .rank > *, .tip > *, .pager span, .src, td.name, td.n, .team-card h2')) {
    if (!rect(el).height) continue;
    const lines = textLines(el);
    if (lines > 1) out.wrapped.push(`${label(el)} "${el.textContent.trim().slice(0, 40)}": ${lines} lines`);
  }
  // The parts of each match card and rank card sit side by side.
  for (const box of document.querySelectorAll('.match, .who, .ranks')) {
    const kids = [...box.children].filter((k) => rect(k).width && getComputedStyle(k).position !== 'absolute');
    for (let i = 0; i < kids.length; i++) for (let j = i + 1; j < kids.length; j++) {
      const a = rect(kids[i]), b = rect(kids[j]);
      const ix = Math.min(a.right, b.right) - Math.max(a.left, b.left);
      const iy = Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top);
      if (ix > 1 && iy > 1) out.overlaps.push(`${label(kids[i])} overlaps ${label(kids[j])}`);
    }
  }
  // Table cells stay table cells, so a row's borders line up.
  for (const td of document.querySelectorAll('td, th')) {
    const d = getComputedStyle(td).display;
    if (d !== 'table-cell' && d !== 'none') out.cells.push(`${label(td)}: display ${d}`);
  }
  for (const img of document.images) if (!img.naturalWidth) out.images.push(img.getAttribute('src'));
  return out;
}

for (const [view, hash, ready] of VIEWS) {
  for (const [device, width] of WIDTHS) {
    test(`${view} at ${device} (${width} px): icons, overflow, wrapping, overlap`, { timeout: 60000 }, async (t) => {
      if (skip) return t.skip(skip);
      const { page, errors } = await open(width, hash, ready);
      try {
        await page.screenshot({ path: new URL(`./screenshots/${view}-${device}.png`, import.meta.url).pathname, fullPage: true });
        const m = await page.evaluate(measure, { icons: ICONS });
        // One report with every kind of problem, rather than stopping at the first.
        const problems = {
          'page errors': errors,
          'icons not at their CSS size': m.icons,
          'page scrolls sideways': m.page.scroll > m.page.width ? [`${m.page.scroll} > ${m.page.width}`] : [],
          'wider than its box': m.overflow,
          'compact lines wrapped': m.wrapped,
          'overlapping parts': m.overlaps,
          'cells not laid out as cells': m.cells,
        };
        assert.deepEqual(Object.fromEntries(Object.entries(problems).filter(([, v]) => v.length)), {});
      } finally {
        await page.close();
      }
    });
  }
}

test('the gold graph tooltip follows the pointer and hides on leave', { timeout: 60000 }, async (t) => {
  if (skip) return t.skip(skip);
  const { page } = await open(1280, VIEWS[2][1], '#goldChart svg');
  try {
    const box = await page.locator('#goldChart svg').boundingBox();
    assert.ok(await page.locator('#gTip').isHidden(), 'hidden until hovered');
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    assert.ok(await page.locator('#gTip').isVisible());
    assert.match(await page.locator('#gTip').textContent(), /1\d min/);
    const tip = await page.locator('#gTip').boundingBox();
    assert.ok(tip.x >= box.x - 1 && tip.x + tip.width <= box.x + box.width + 1, 'tooltip stays inside the chart');
    await page.mouse.move(box.x + box.width - 2, box.y + box.height / 2);
    const right = await page.locator('#gTip').boundingBox();
    assert.ok(right.x + right.width <= box.x + box.width + 1, 'tooltip stays inside the chart at the right edge');
    await page.mouse.move(0, 0);
    assert.ok(await page.locator('#gTip').isHidden());
  } finally {
    await page.close();
  }
});

test('images the mirror cannot serve keep their box', { timeout: 60000 }, async (t) => {
  if (skip) return t.skip(skip);
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  try {
    await page.addInitScript(() => localStorage.setItem('rp.showcase.platform', '"oc1"'));
    await page.route('http://showcase.test/**', (r) => {
      const u = new URL(r.request().url());
      if (u.pathname === '/dev/showcase') return r.fulfill({ contentType: 'text/html', body: html });
      if (u.pathname.startsWith('/ddragon/')) return r.fulfill({ status: 404, body: '' });
      const [status, body] = api([], u.pathname + u.search, OPTS);
      return r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
    });
    await page.goto('http://showcase.test/dev/showcase#/player/Faker/KR1');
    await page.waitForSelector('#pMatches .match');
    await page.waitForLoadState('networkidle');
    const m = await page.evaluate(measure, { icons: ICONS });
    assert.deepEqual(m.icons, []);
    assert.equal(await page.locator('#pMatches .match img').count(), 0, 'every failed image became a box');
  } finally {
    await page.close();
  }
});
