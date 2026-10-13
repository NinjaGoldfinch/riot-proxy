// The About tab (ADM-15) in jsdom: /v1/admin/info as a page (build, uptime,
// features), readiness, links, and the configuration in effect, searchable,
// with secrets as the server redacted them.
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { page, KEY } from './admin-harness.mjs';
import { INFO } from './fake-admin.mjs';

test('the build, uptime, environment and features', async () => {
  const p = await page({ key: KEY, hash: '#about' });
  try {
    const kv = p.words('#panel-about dl.kv');
    assert.match(kv, /^version 2\.3\.0 commit 41567bd0c0ff compiler rustc 1\.98\.1 built .+ started .+ · up 3d 4h environment production · role all · 8 workers key scope 5f3a9c1e77aa$/);
    const feats = p.$$('#panel-about ul.groups li').map((li) => [...li.children].map((c) => c.textContent.trim()).join(' ').trim());
    assert.ok(feats.includes('docsUi on') && feats.includes('devUi off'), feats.join('|'));
    assert.ok(p.$('#panel-about ul.groups li [data-level="good"] svg use'), 'on/off with an icon and a word');
    assert.deepEqual(JSON.parse(p.text('#panel-about pre.payload')), { ok: true, sqlite: true, limiter: true, readers: 6, readersFree: 6 });
    assert.deepEqual(p.$$('#panel-about a').map((a) => a.getAttribute('href')), ['http://admin.test/metrics', 'http://admin.test/docs']);
  } finally { p.close(); }
});

test('the configuration is listed in full, searchable, with secrets redacted by the server', async () => {
  const p = await page({ key: KEY, hash: '#about' });
  try {
    const rows = () => p.$$('#abConfig tbody tr').map((tr) => [...tr.children].map((c) => c.textContent));
    assert.deepEqual(rows(), Object.entries(INFO.config).sort(([a], [b]) => a.localeCompare(b)));
    assert.ok(!p.$('#panel-about').textContent.includes('RIOT_API_KEY='));
    p.$('#abSearch').value = 'redacted';
    p.$('#abSearch').dispatchEvent(new p.w.Event('input'));
    assert.deepEqual(rows(), [['BOOTSTRAP_ADMIN_KEY', '[REDACTED]']]);
    p.$('#abSearch').value = 'port';
    p.$('#abSearch').dispatchEvent(new p.w.Event('input'));
    assert.deepEqual(rows(), [['PORT', '8080']]);
  } finally { p.close(); }
});
