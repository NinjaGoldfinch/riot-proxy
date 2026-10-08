// The dashboard's Ladder tab in headless Chromium (DEV-13), at a desktop, a tablet
// and a phone width, with a running and a finished crawl opened in the history:
// the page never scrolls sideways, nothing is wider than its panel, and the stage
// bars and job lists keep to one line per entry. Screenshots go to screenshots/.
// Skips without a browser, except under CI (see showcase.browser.test.mjs).
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync } from 'node:fs';
import { chromium } from 'playwright-core';

import { html, RUNNING, DONE, api } from './fake-dashboard.mjs';

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
  test(`ladder tab at ${device} (${width} px): open runs fit their panels`, { timeout: 60000 }, async (t) => {
    if (skip) return t.skip(skip);
    const page = await browser.newPage({ viewport: { width, height: 900 } });
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));
    try {
      await page.route('http://dashboard.test/**', (r) => {
        const u = new URL(r.request().url());
        if (u.pathname === '/dashboard') return r.fulfill({ contentType: 'text/html', body: html });
        const [status, body] = api([], u.pathname + u.search, { method: r.request().method() });
        return r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
      });
      await page.goto('http://dashboard.test/dashboard#ladder');
      await page.waitForSelector(`tr[data-run="${RUNNING}"]`);
      await page.click(`tr[data-run="${RUNNING}"] td`);
      await page.click(`tr[data-run="${DONE}"] td`);
      await page.waitForSelector(`[data-activity="${RUNNING}"] .stages`);
      await page.waitForSelector(`[data-activity="${DONE}"] .stages`);
      await page.screenshot({ path: new URL(`./screenshots/dashboard-ladder-${device}.png`, import.meta.url).pathname, fullPage: true });
      const m = await page.evaluate(() => {
        const out = { page: [document.documentElement.scrollWidth, innerWidth], overflow: [], wrapped: [] };
        for (const el of document.querySelectorAll('#tab-ladder .panel, .activity, .stages, .activity-cols > div, .queue-cols > div')) {
          if (el.closest('.scroll') && !el.matches('.activity, .stages, .activity-cols > div')) continue;
          if (el.scrollWidth > el.clientWidth + 1) out.overflow.push(`${el.className || el.tagName}: ${el.scrollWidth} > ${el.clientWidth}`);
        }
        for (const el of document.querySelectorAll('.jobs li:not(.fail), .jobs li.fail .err, .legs span')) {
          const r = el.getBoundingClientRect();
          const lh = parseFloat(getComputedStyle(el).lineHeight) || 18;
          if (r.height > lh * 1.6 && !el.matches('.stages .num')) out.wrapped.push(el.textContent.trim().slice(0, 50));
        }
        // In view, not just inside its box: the detail sits in a scrolling table.
        out.offscreen = [];
        for (const el of document.querySelectorAll('.activity, .stages .num, .activity-cols > div, .jobs li')) {
          const r = el.getBoundingClientRect();
          if (r.width && (r.left < -1 || r.right > innerWidth + 1)) out.offscreen.push(`${el.className || el.tagName} "${el.textContent.trim().slice(0, 30)}": ${Math.round(r.left)}–${Math.round(r.right)}`);
        }
        return out;
      });
      assert.deepEqual(errors, []);
      assert.ok(m.page[0] <= m.page[1], `page scrolls sideways: ${m.page[0]} > ${m.page[1]}`);
      assert.deepEqual(m.overflow, [], 'nothing wider than its box');
      assert.deepEqual(m.wrapped, [], 'job rows and leg chips stay on one line');
      assert.deepEqual(m.offscreen, [], 'every part of an opened run is on screen');
    } finally {
      await page.close();
    }
  });
}
