// The Cache tab (ADM-10) in jsdom: the tiers' sizes and reads, the 24 h hit
// ratio, the probe (/v1/admin/debug/cache) and a purge that must be counted
// (a dry run) and confirmed before it deletes, then says how many went.
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { page, KEY } from './admin-harness.mjs';
import { scene, snapshot, limiterSnapshot } from './fake-admin.mjs';

const cachePage = async () => {
  scene.snapshot = limiterSnapshot();
  scene.cacheKeys = ['5f3a:summoner.byPuuid:kr:a', '5f3a:summoner.byPuuid:kr:b', '5f3a:league.entries:kr:c'];
  return page({ key: KEY, hash: '#cache' });
};

test('tiles: memory entries and use as a meter, persisted rows, dropped writes, reads', async () => {
  const p = await cachePage();
  try {
    const tiles = p.$$('#cacheTiles .tile').map((t) => t.textContent.replace(/\s+/g, ' ').trim());
    assert.equal(tiles[0], 'Memory entries12.4K100 MB of 128 MB');
    assert.match(tiles[1], /^Memory used78 %/);
    assert.equal(p.$('#cacheTiles .meter').getAttribute('aria-valuetext'), '100 MB of 128 MB');
    assert.equal(p.$('#cacheTiles .meter .mt').textContent.trim(), '100 MB / 128 MB');
    assert.match(tiles[2], /^Persisted rows3,210/);
    assert.match(tiles[3], /^Dropped writes7the write-behind queue was full$/);
    assert.ok(p.$$('#cacheTiles .tile')[3].querySelector('.label svg use'), 'dropped writes are flagged with an icon');
    assert.match(tiles[4], /^Hit ratio since start95\.0 %940 hit · 50 miss · 5 neg · 5 stale$/);
    assert.equal(p.$$('#chCacheHit path.line').length, 1, 'the 24 h hit ratio');
  } finally { p.close(); scene.snapshot = snapshot(); }
});

test('the probe shows the key, whether it is cached, its age and freshness, or why not', async () => {
  const p = await cachePage();
  try {
    p.$('#probeScope').value = 'kr';
    p.$('#probeMethod').value = 'summoner.byPuuid';
    p.$('#probePath').value = '/lol/summoner/v4/summoners/by-puuid/x';
    p.$('#probeForm').dispatchEvent(new p.w.Event('submit', { cancelable: true }));
    await p.settle();
    const call = p.calls.find((c) => c.url.includes('/v1/admin/debug/cache'));
    assert.ok(call.url.includes('scope=kr') && call.url.includes('method=summoner.byPuuid') && call.url.includes('path=%2Flol%2Fsummoner'));
    assert.equal(p.text('#probeResult'), 'key5f3a:summoner.byPuuid:kr:xcachedyesage2m 5sstatefresh');
    p.$('#probeMethod').value = '';
    p.$('#probeForm').dispatchEvent(new p.w.Event('submit', { cancelable: true }));
    await p.settle();
    assert.equal(p.text('#probeResult'), "querystring must have required property 'method'");
  } finally { p.close(); scene.snapshot = snapshot(); }
});

test('a purge is counted first, then confirmed, then reports how many went', async () => {
  const p = await cachePage();
  try {
    assert.equal(p.$('#purgeGo').disabled, true, 'nothing to purge before a count');
    p.$('#purgePattern').value = 'summoner.byPuuid:*';
    p.$('#purgePattern').dispatchEvent(new p.w.Event('input'));
    p.$('#purgeForm').dispatchEvent(new p.w.Event('submit', { cancelable: true }));
    await p.settle();
    const dry = p.calls.find((c) => c.url.endsWith('/v1/admin/cache/purge'));
    assert.deepEqual(JSON.parse(dry.body), { pattern: 'summoner.byPuuid:*', dryRun: true });
    assert.equal(p.text('#purgeMeta'), '2 entries match summoner.byPuuid:*.');
    assert.equal(p.$('#purgeGo').disabled, false);
    assert.equal(p.text('#purgeGo'), 'Purge 2 entries');
    assert.equal(scene.cacheKeys.length, 3, 'the count deleted nothing');

    // Declined: nothing is deleted.
    p.$('#purgeGo').click();
    await p.settle();
    assert.equal(p.$('#confirm').hidden, false);
    assert.match(p.text('#confirmText'), /^Delete 2 cached entries matching summoner\.byPuuid:\*\?/);
    p.$('#confirmCancel').click();
    await p.settle();
    assert.equal(p.calls.filter((c) => c.url.endsWith('/v1/admin/cache/purge')).length, 1);

    // Confirmed: deleted and reported.
    p.$('#purgeGo').click();
    await p.settle();
    p.$('#confirmOk').click();
    await p.settle();
    const real = p.calls.filter((c) => c.url.endsWith('/v1/admin/cache/purge'))[1];
    assert.deepEqual(JSON.parse(real.body), { pattern: 'summoner.byPuuid:*' });
    assert.deepEqual(scene.cacheKeys, ['5f3a:league.entries:kr:c']);
    assert.equal(p.text('#toasts'), 'Purged 2 entries matching summoner.byPuuid:*.');
    assert.equal(p.$('#purgeGo').disabled, true, 'a new purge needs a new count');
  } finally { p.close(); scene.snapshot = snapshot(); }
});

test('editing the pattern after a count disarms the purge', async () => {
  const p = await cachePage();
  try {
    p.$('#purgePattern').value = 'summoner.byPuuid:*';
    p.$('#purgeForm').dispatchEvent(new p.w.Event('submit', { cancelable: true }));
    await p.settle();
    assert.equal(p.$('#purgeGo').disabled, false);
    p.$('#purgePattern').value = '*';
    p.$('#purgePattern').dispatchEvent(new p.w.Event('input'));
    assert.equal(p.$('#purgeGo').disabled, true);
  } finally { p.close(); scene.snapshot = snapshot(); }
});
