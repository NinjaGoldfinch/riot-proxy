// The dashboard in headless Chromium (ADM-08, ADM-09) at a desktop, a tablet
// and a phone width, in both themes: no view scrolls sideways, the health
// strip, tiles, charts and panels fit their boxes, a chart's crosshair follows
// the pointer, and every tab can be reached and
// opened (the rail at 820 px and up, a scrolling tab strip below). Screenshots
// go to screenshots/. Skips without a browser, except under CI (see
// showcase.browser.test.mjs).
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync } from 'node:fs';
import { chromium } from 'playwright-core';

import { html, api, KEY, scene, limiterSnapshot, ladderSnapshot } from './fake-admin.mjs';

// Every scope, meter and fold the Limiter and Cache tabs draw, and a running crawl.
scene.snapshot = { ...limiterSnapshot(), ladder: ladderSnapshot().ladder, analytics: ladderSnapshot().analytics };

let browser = null;
let skip = false;
before(async () => {
  try {
    browser = await chromium.launch(process.env.CHROMIUM_PATH ? { executablePath: process.env.CHROMIUM_PATH } : {});
  } catch (e) {
    if (process.env.CI) throw e;
    skip = `no headless Chromium (${String(e.message).split('\n')[0]})`;
  }
  mkdirSync(new URL('./screenshots/', import.meta.url), { recursive: true });
});
after(async () => { await browser?.close(); });

for (const [device, width] of [['desktop', 1280], ['tablet', 820], ['phone', 390]]) {
  for (const theme of ['dark', 'light']) {
    test(`dashboard shell at ${device} (${width} px), ${theme}: fits, and every tab is reachable`, { timeout: 60000 }, async (t) => {
      if (skip) return t.skip(skip);
      const page = await browser.newPage({ viewport: { width, height: 900 } });
      const errors = [];
      page.on('pageerror', (e) => errors.push(e.message));
      try {
        await page.addInitScript(([key, theme]) => {
          localStorage.setItem('rp.dashboard.key', key);
          localStorage.setItem('rp.admin.theme', theme);
          // No server behind the fake: a socket that never opens.
          window.WebSocket = class { constructor() { this.readyState = 0; } send() {} close() {} };
        }, [KEY, theme]);
        await page.route('http://admin.test/**', (r) => {
          const u = new URL(r.request().url());
          if (u.pathname === '/dashboard') return r.fulfill({ contentType: 'text/html', body: html });
          const [status, body] = api([], u.pathname + u.search, { method: r.request().method(), auth: r.request().headers().authorization ?? null });
          return r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
        });
        await page.goto('http://admin.test/dashboard');
        await page.waitForSelector('#tiles .tile');
        await page.waitForSelector('#chUpstream .plotwrap');
        // Each view: nothing sideways, nothing wider than its box (tables scroll inside .scroll).
        const fits = () => page.evaluate(() => {
          const out = { page: [document.documentElement.scrollWidth, innerWidth], overflow: [] };
          for (const el of document.querySelectorAll('.strip, .tile, .tile .value, .rail-foot, main, .chart, .chart figcaption, .xaxis, .panel, .filters, .meter, .scope header, .form-row, .worker, ul.groups li, ol.timeline li, .crawl, .stages, .totals, .legs, .stack, .legend, .note-line, .newkey, .chips, ol.events li')) {
            if (el.scrollWidth > el.clientWidth + 1) out.overflow.push(`${el.id || el.className || el.tagName}: ${el.scrollWidth} > ${el.clientWidth}`);
          }
          return out;
        });
        const ready = { overview: '#chArchive .plotwrap', traffic: '#trRecent tbody tr', limiter: '.scope[data-scope="oc1"] .meter', cache: '#chCacheHit .plotwrap', jobs: '#jNext ul.groups li', 'jobs/01K00000000000000000000RUN': '#jtTimeline li', ladder: '[data-activity="01K0000000000000000000RUN1"] .stages', 'ladder/01K0000000000000000000DON1': '#ladderHistory [data-activity="01K0000000000000000000DON1"] .stages', archive: '#arTimelines .meter', consumers: '#plList tbody tr', events: '#evList', about: '#abConfig tbody tr' };
        for (const view of Object.keys(ready)) {
          await page.evaluate((v) => { location.hash = v; }, view);
          await page.waitForSelector(ready[view]);
          if (view === 'limiter') {
            // Unfold everything, the live reading included.
            for (const s of await page.$$('.scope details summary')) await s.click();
            await page.waitForSelector('details[data-live="kr"] .live .meter');
          }
          if (view === 'events') {
            // Frames as the firehose sends them, one opened into its JSON.
            await page.evaluate(() => { for (let i = 0; i < 40; i++) window.rpAdmin.store.set('event', { op: 'event', event: i % 2 ? 'match.archived' : 'crawl.phase', topic: 'firehose', at: Date.now(), data: { matchId: `OC1_${i}`, note: 'x'.repeat(i * 4) } }); });
            await page.waitForSelector('#evList li button');
            await page.click('#evList li button');
          }
          if (view === 'ladder') {
            for (const s of await page.$$('#ladderRunning details.more summary, #analyticsFold summary')) await s.click();
          }
          await page.screenshot({ path: new URL(`./screenshots/admin-${view.split('/')[0]}${view.includes('/') ? '-trace' : ''}-${device}-${theme}.png`, import.meta.url).pathname, fullPage: true });
          const f = await fits();
          if (view.startsWith('ladder/')) {
            // Every part of an opened run is on screen, not just inside its scrolling table.
            const off = await page.evaluate(() => [...document.querySelectorAll('#ladderHistory .activity, #ladderHistory .stages, #ladderHistory .totals')]
              .filter((el) => { const r = el.getBoundingClientRect(); return r.width && (r.left < -1 || r.right > innerWidth + 1); })
              .map((el) => el.className));
            assert.deepEqual(off, [], `${view}: the opened run is on screen`);
          }
          assert.ok(f.page[0] <= f.page[1], `${view}: page scrolls sideways: ${f.page[0]} > ${f.page[1]}`);
          assert.deepEqual(f.overflow, [], `${view}: nothing wider than its box`);
        }
        // A chart's crosshair follows the pointer.
        await page.evaluate(() => { location.hash = 'overview'; });
        await page.waitForSelector('#chBacklog .plotwrap');
        await page.hover('#chBacklog .plotwrap');
        assert.match(await page.textContent('#chBacklog .readout'), /^\d\d:\d\d · /);
        const m = await page.evaluate(() => {
          const out = { page: [document.documentElement.scrollWidth, innerWidth], overflow: [], tabs: [] };
          const strip = document.querySelector('#tabs');
          for (const b of document.querySelectorAll('[role="tab"]')) {
            b.scrollIntoView({ block: 'nearest', inline: 'nearest' });
            const r = b.getBoundingClientRect();
            const s = strip.getBoundingClientRect();
            out.tabs.push({ id: b.id, visible: r.width > 0 && r.left >= s.left - 1 && r.right <= s.right + 1 && r.left >= 0 && r.right <= innerWidth });
          }
          return out;
        });
        assert.deepEqual(errors, []);
        assert.ok(m.page[0] <= m.page[1], `page scrolls sideways: ${m.page[0]} > ${m.page[1]}`);
        assert.deepEqual(m.overflow, [], 'nothing wider than its box');
        assert.ok(m.tabs.length >= 1);
        assert.deepEqual(m.tabs.filter((x) => !x.visible), [], 'every tab can be scrolled into view');
        for (const { id } of m.tabs) {
          await page.click(`#${id}`);
          await page.waitForFunction((tab) => document.getElementById(tab).getAttribute('aria-selected') === 'true', id);
          assert.equal(await page.isVisible(`#${id.replace('tab-', 'panel-')}`), true, id);
        }
        // The rail is a column on wide screens, a row of tabs on narrow ones.
        const dir = await page.$eval('#tabs', (el) => getComputedStyle(el).flexDirection);
        assert.equal(dir, width >= 820 ? 'column' : 'row');
      } finally {
        await page.close();
      }
    });
  }
}
