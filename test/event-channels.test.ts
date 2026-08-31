import './helpers/env.js';
import { describe, expect, it } from 'vitest';
import { KEY_SCOPE } from '../src/config.js';
import { CHANNEL_PATTERN, channelFor, topicFromChannel } from '../src/events/index.js';
import { FIREHOSE_TOPIC, METRICS_TOPIC, PATCH_TOPIC, playerTopic } from '../src/events/topics.js';

/**
 * §7.4 / §11 — the wire mapping, in isolation from the socket path.
 *
 * The bug this pins (#83): channels were a flat `evt:<topic>` while every Redis
 * *key* carried `KEY_SCOPE`, so a test suite and a dev server sharing one Redis
 * relayed each other's events into each other's dashboards.
 */

/** Does Redis' `PSUBSCRIBE` glob match this channel? Only `*` is in play. */
function patternMatches(pattern: string, channel: string): boolean {
  const parts = pattern.split('*').map((p) => p.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'));
  return new RegExp(`^${parts.join('.*')}$`).test(channel);
}

describe('event channels (§11)', () => {
  it('namespaces every channel by the key scope', () => {
    // Spelled out rather than composed from the module's own prefix, so the
    // wire format itself is pinned and not just checked against itself.
    expect(channelFor(PATCH_TOPIC)).toBe(`evt:${KEY_SCOPE}:patch`);
    expect(channelFor(playerTopic('P1'))).toBe(`evt:${KEY_SCOPE}:player:P1`);
  });

  it('leaves the topic itself unscoped — it is the client-facing name', () => {
    // A consumer subscribes to `patch`, not to `patch` under our key's hash;
    // the scope exists only between publisher and relay.
    for (const topic of [PATCH_TOPIC, METRICS_TOPIC, FIREHOSE_TOPIC, playerTopic('P1')]) {
      expect(topicFromChannel(channelFor(topic))).toBe(topic);
    }
  });

  it('recovers a topic containing colons', () => {
    // `player:<puuid>` is the common case, and the scope sits in front of it.
    expect(topicFromChannel(`evt:${KEY_SCOPE}:player:P1`)).toBe('player:P1');
  });

  it('subscribes to this deployment only', () => {
    expect(patternMatches(CHANNEL_PATTERN, channelFor(PATCH_TOPIC))).toBe(true);

    // Another deployment on the same Redis, and the unscoped channel this
    // service published on before the fix. Neither is ours.
    expect(patternMatches(CHANNEL_PATTERN, 'evt:0000dead:patch')).toBe(false);
    expect(patternMatches(CHANNEL_PATTERN, 'evt:patch')).toBe(false);
  });
});
