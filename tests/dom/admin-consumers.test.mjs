// The Consumers & Players tab (ADM-14) in jsdom: consumers with their quota
// use from the snapshot; creating one shows its key once, with a copy button
// and a warning, and keeps it nowhere; revoking (the page's own key opens the
// key sheet); dropping a key from the auth cache by its SHA-256, hashed in the
// page; tracked players, tracked and untracked. Every write asks first.
import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';

import { page, KEY } from './admin-harness.mjs';
import { scene, snapshot, consumersWorld, ADMIN_ID } from './fake-admin.mjs';

beforeEach(() => {
  scene.consumers = consumersWorld();
  scene.snapshot = snapshot({ quotas: [{ consumer: ADMIN_ID, limit: 10000, used: 42, resetSecs: 60 }] });
});
const submit = async (p, form) => { p.$(form).dispatchEvent(new p.w.Event('submit', { cancelable: true })); await p.settle(); };
const confirm = async (p) => { p.$('#confirmOk').click(); await p.settle(); };

test('consumers: active first, quota use as a meter, revoked ones marked', async () => {
  const p = await page({ key: KEY, hash: '#consumers' });
  try {
    assert.equal(p.text('#cnMeta'), '2 active · 1 revoked');
    const rows = p.$$('#cnList tbody tr');
    assert.deepEqual(rows.map((r) => r.firstElementChild.firstChild.textContent), ['ops', 'ninjagoldfinch.lol', 'old-site']);
    assert.equal(rows[0].querySelector('.meter').getAttribute('aria-valuetext'), '42 of 10,000');
    assert.equal(rows[1].children[2].textContent, '0 / 600 a minute');
    assert.match(rows[2].textContent, /revoked 60d ago/);
    assert.equal(rows[2].querySelector('[data-revoke]'), null);
    assert.deepEqual([...p.$('#cnRecheckId').options].map((o) => o.textContent), ['ops', 'ninjagoldfinch.lol'], 'only active keys can be re-checked');
  } finally { p.close(); }
});

test('a new consumer shows its key once, with copy and a warning, and the key is kept nowhere', async () => {
  const p = await page({ key: KEY, hash: '#consumers' });
  try {
    p.$('#cnName').value = 'mon';
    p.$('#cnAdmin').checked = true;
    p.$('#cnQuota').value = '120';
    await submit(p, '#cnCreate');
    assert.equal(p.text('#confirmText'), 'Create mon with read and admin scopes and 120 requests a minute? Its key is shown once.');
    await confirm(p);
    assert.deepEqual(scene.consumers.bodies[0], ['/v1/admin/consumers', { name: 'mon', scopes: ['read', 'admin'], quotaPerMin: 120 }]);
    assert.equal(p.text('#cnKeyText'), 'rpx_NEWKEY_shown_once_0123456789');
    assert.match(p.text('#cnKey .newkey'), /Store this key now/);
    p.$('#cnCopy').click();
    await p.settle();
    assert.deepEqual(p.w.copied, ['rpx_NEWKEY_shown_once_0123456789']);
    // Nowhere else: not in storage, not in a toast, not in the store.
    for (let i = 0; i < p.w.localStorage.length; i++) assert.ok(!p.w.localStorage.getItem(p.w.localStorage.key(i)).includes('NEWKEY'));
    assert.ok(!p.text('#toasts').includes('NEWKEY'));
    assert.ok(!JSON.stringify(p.w.rpAdmin.store.data).includes('NEWKEY'));
    assert.ok(p.$$('#cnList tbody tr').some((r) => r.textContent.includes('mon')), 'the list is refreshed');
    p.$('#cnDone').click();
    assert.equal(p.$('#cnKeyText'), null, 'Done removes it from the page');
  } finally { p.close(); }
});

test('revoking a consumer asks first; revoking the key in use opens the key sheet', async () => {
  const p = await page({ key: KEY, hash: '#consumers' });
  try {
    p.$('[data-revoke="01K0000000000000000000WEB1"]').click();
    await p.settle();
    assert.equal(p.text('#confirmText'), 'Revoke ninjagoldfinch.lol? Its key stops working at once; the row is kept.');
    await confirm(p);
    assert.equal(p.text('#toasts'), 'ninjagoldfinch.lol revoked.');
    assert.equal(p.text('#cnMeta'), '1 active · 2 revoked');
    assert.equal(p.$('#sheet').hidden, true);

    p.$(`[data-revoke="${ADMIN_ID}"]`).click();
    await p.settle();
    await confirm(p);
    assert.equal(p.$('#sheet').hidden, false, "this page's own key was revoked: the sheet asks for another");
  } finally { p.close(); }
});

test('a key is re-checked by its SHA-256, hashed in the page; a hash is sent as given', async () => {
  const p = await page({ key: KEY, hash: '#consumers' });
  try {
    p.$('#cnRecheckId').value = '01K0000000000000000000WEB1';
    p.$('#cnRecheckKey').value = 'rpx_site_key';
    await submit(p, '#cnRecheck');
    await confirm(p);
    const want = createHash('sha256').update('rpx_site_key').digest('hex');
    assert.deepEqual(scene.consumers.bodies.at(-1), ['/v1/admin/consumers/01K0000000000000000000WEB1/revoke-cache', { keyHash: want }]);
    assert.equal(p.$('#cnRecheckKey').value, '', 'the key does not stay in the field');
    assert.equal(p.text('#toasts'), 'Dropped from the cache; the key is still active.');
    p.$('#cnRecheckKey').value = want.toUpperCase();
    await submit(p, '#cnRecheck');
    await confirm(p);
    assert.deepEqual(scene.consumers.bodies.at(-1)[1], { keyHash: want });
  } finally { p.close(); }
});

test('tracked players: listed with their history, tracked by Riot ID or PUUID, untracked after a confirm', async () => {
  const p = await page({ key: KEY, hash: '#consumers' });
  try {
    assert.ok(p.calls.some((c) => c.url.endsWith('/v1/admin/tracked-players?tracked=true')), 'only tracked rows are fetched');
    assert.equal(p.text('#plMeta'), '1 tracked of 41,200 known');
    assert.match(p.words('#plList tbody tr'), /^Hide on bush #KR1 kr KR_1 walked 58m ago Untrack$/);
    p.$('#plPlatform').value = 'oc1';
    p.$('#plWho').value = 'Some One#OCE';
    await submit(p, '#plTrack');
    assert.match(p.text('#confirmText'), /^Track Some One#OCE on oc1\?/);
    await confirm(p);
    assert.deepEqual(scene.consumers.bodies.at(-1), ['/v1/admin/tracked-players', { platform: 'oc1', gameName: 'Some One', tagLine: 'OCE' }]);
    assert.equal(p.$$('#plList tbody tr').length, 2);
    p.$('#plWho').value = 'Q'.repeat(78);
    await submit(p, '#plTrack');
    await confirm(p);
    assert.deepEqual(scene.consumers.bodies.at(-1)[1], { platform: 'oc1', puuid: 'Q'.repeat(78) });
    p.$(`[data-untrack="${'P'.repeat(78)}"]`).click();
    await p.settle();
    await confirm(p);
    assert.ok(!p.$$('#plList tbody tr').some((r) => r.textContent.includes('Hide on bush')), 'untracked rows leave the list');
  } finally { p.close(); }
});
