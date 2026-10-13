// Keyboard use (ADM-16) in jsdom: `/` opens the command palette (tabs,
// actions, a pasted job or crawl id), arrows and Enter drive it, Esc closes
// it and gives focus back; `?` shows the keys; 1–9 and 0 open tabs; `t`
// switches the theme; Tab stays inside an open dialog; nothing fires while
// typing in a field. Every interactive element has a name.
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { page, KEY } from './admin-harness.mjs';

const key = (p, k, target, opts = {}) => (target ?? p.w.document.body).dispatchEvent(new p.w.KeyboardEvent('keydown', { key: k, bubbles: true, cancelable: true, ...opts }));

test('/ opens the palette; typing filters; arrows and Enter go to a tab; focus returns', async () => {
  const p = await page({ key: KEY });
  try {
    p.$('#tab-overview').focus();
    key(p, '/');
    assert.equal(p.$('#palette').hidden, false);
    assert.equal(p.w.document.activeElement, p.$('#palInput'));
    const input = p.$('#palInput');
    input.value = 'go jo';
    input.dispatchEvent(new p.w.Event('input'));
    assert.deepEqual(p.$$('#palList li').map((li) => li.firstChild.textContent), ['Go to Jobs']);
    input.value = 'go';
    input.dispatchEvent(new p.w.Event('input'));
    assert.equal(p.$$('#palList li').length, 10, 'every tab');
    key(p, 'ArrowDown', input);
    key(p, 'ArrowDown', input);
    assert.equal(p.$('#palList li[aria-selected="true"]').firstChild.textContent, 'Go to Limiter');
    assert.equal(input.getAttribute('aria-activedescendant'), 'pal-2');
    key(p, 'Enter', input);
    await p.settle();
    assert.equal(p.$('#palette').hidden, true);
    assert.equal(p.w.location.hash, '#limiter');
    assert.equal(p.w.document.activeElement, p.$('#tab-overview'), 'focus goes back where it was');
  } finally { p.close(); }
});

test('a pasted ULID offers to open it as a job or a crawl', async () => {
  const p = await page({ key: KEY });
  try {
    key(p, '/');
    const input = p.$('#palInput');
    input.value = '01K00000000000000000000RUN';
    input.dispatchEvent(new p.w.Event('input'));
    assert.deepEqual(p.$$('#palList li').map((li) => li.firstChild.textContent), ['Open job 01K00000000000000000000RUN', 'Open crawl 01K00000000000000000000RUN']);
    key(p, 'Enter', input);
    await p.settle();
    assert.equal(p.w.location.hash, '#jobs/01K00000000000000000000RUN');
  } finally { p.close(); }
});

test('actions run from the palette; Esc closes it without running anything', async () => {
  const p = await page({ key: KEY });
  try {
    key(p, '/');
    const input = p.$('#palInput');
    input.value = 'theme';
    input.dispatchEvent(new p.w.Event('input'));
    key(p, 'Enter', input);
    assert.equal(p.w.document.documentElement.dataset.theme, 'light');
    key(p, '/');
    input.value = 'sign';
    input.dispatchEvent(new p.w.Event('input'));
    key(p, 'Escape', input);
    assert.equal(p.$('#palette').hidden, true);
    assert.equal(p.$('#sheet').hidden, true, 'nothing ran');
  } finally { p.close(); }
});

test('? shows the keys; digits open tabs; t switches the theme; typing in a field fires nothing', async () => {
  const p = await page({ key: KEY });
  try {
    key(p, '?');
    assert.equal(p.$('#help').hidden, false);
    assert.match(p.text('#help dl'), /command palette/);
    key(p, 'Escape');
    assert.equal(p.$('#help').hidden, true);
    key(p, '3');
    await p.settle();
    assert.equal(p.w.location.hash, '#limiter');
    assert.equal(p.w.document.activeElement, p.$('#tab-limiter'));
    key(p, '0');
    await p.settle();
    assert.equal(p.w.location.hash, '#about', '0 is the tenth tab');
    key(p, 't');
    assert.equal(p.w.document.documentElement.dataset.theme, 'light');
    // In a field, keys are text.
    p.w.location.hash = '#traffic';
    await p.settle();
    key(p, '/', p.$('#trText'));
    key(p, '2', p.$('#trText'));
    assert.equal(p.$('#palette').hidden, true);
    assert.equal(p.w.location.hash, '#traffic');
    // With a modifier, keys are the browser's.
    key(p, '1', null, { ctrlKey: true });
    assert.equal(p.w.location.hash, '#traffic');
  } finally { p.close(); }
});

test('Tab stays inside an open dialog, both ways', async () => {
  const p = await page({ key: KEY });
  try {
    const done = p.w.rpAdmin.confirmAction('Sure?');
    const [cancel, ok] = [p.$('#confirmCancel'), p.$('#confirmOk')];
    ok.focus();
    key(p, 'Tab', ok);
    assert.equal(p.w.document.activeElement, cancel, 'past the last, back to the first');
    key(p, 'Tab', cancel, { shiftKey: true });
    assert.equal(p.w.document.activeElement, ok, 'before the first, round to the last');
    key(p, '3', ok);
    assert.equal(p.w.location.hash, '', 'shortcuts wait while a dialog is open');
    key(p, 'Escape', ok);
    assert.equal(await done, false);
  } finally { p.close(); }
});

test('every interactive element on every tab has a name', async () => {
  const p = await page({ key: KEY });
  try {
    const nameOf = (el) => {
      const d = p.w.document;
      const byIds = (ids) => ids.split(/\s+/).map((id) => d.getElementById(id)?.textContent ?? '').join(' ');
      return (el.getAttribute('aria-label') || (el.getAttribute('aria-labelledby') && byIds(el.getAttribute('aria-labelledby'))) ||
        (el.id && d.querySelector(`label[for="${el.id}"]`)?.textContent) || el.closest('label')?.textContent || el.textContent || el.getAttribute('title') || '').trim();
    };
    const missing = [];
    for (const id of ['overview', 'traffic', 'limiter', 'cache', 'jobs', 'ladder', 'archive', 'consumers', 'events', 'about']) {
      p.w.location.hash = `#${id}`;
      await p.settle();
      for (const el of p.$$('button, a[href], input, select, [role="tab"], [tabindex="0"]')) {
        if (el.closest('[hidden]')) continue;
        if (!nameOf(el)) missing.push(`${id}: ${el.outerHTML.slice(0, 90)}`);
      }
    }
    assert.deepEqual(missing, []);
  } finally { p.close(); }
});
