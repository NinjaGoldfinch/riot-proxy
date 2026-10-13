// The dashboard in headless Chromium (ADM-08) at a desktop, a tablet and a
// phone width, in both themes: the page never scrolls sideways, the health
// strip and the tiles fit their boxes, and every tab can be reached and
// opened (the rail at 820 px and up, a scrolling tab strip below). Screenshots
// go to screenshots/. Skips without a browser, except under CI (see
// showcase.browser.test.mjs).
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync } from 'node:fs';
import { chromium } from 'playwright-core';

import { html, api, KEY } from './fake-admin.mjs';

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
        await page.screenshot({ path: new URL(`./screenshots/admin-${device}-${theme}.png`, import.meta.url).pathname, fullPage: true });
        const m = await page.evaluate(() => {
          const out = { page: [document.documentElement.scrollWidth, innerWidth], overflow: [], tabs: [] };
          for (const el of document.querySelectorAll('.strip, .tile, .tile .value, .rail-foot, main')) {
            if (el.scrollWidth > el.clientWidth + 1) out.overflow.push(`${el.className || el.tagName}: ${el.scrollWidth} > ${el.clientWidth}`);
          }
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
          assert.equal(await page.getAttribute(`#${id}`, 'aria-selected'), 'true', id);
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
