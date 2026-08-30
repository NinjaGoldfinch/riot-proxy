/**
 * Lua kept inline rather than in `.lua` files: `tsc` does not copy non-TS
 * assets, and a build that silently ships without its limiter scripts is a
 * worse failure than slightly noisier source.
 */

/**
 * Atomic acquisition across every applicable Riot bucket (§9.2).
 *
 * Each bucket is a sorted set of admission timestamps, trimmed to the window on
 * every call, so the constraint enforced is "at most `limit` in the last
 * `window` seconds" — true at every instant rather than only within our own
 * window.
 *
 * The counter this replaces assumed our window and Riot's started together,
 * which only holds after a full idle window. Under sustained traffic they drift
 * apart, and INCR + EXPIRE-on-first-hit then admits up to 2x the limit across a
 * boundary: the tail of one window and the head of the next both land inside
 * one of Riot's seconds. That is an accountable 429 by construction, and no
 * amount of count-syncing prevents it, because the overshoot is already on the
 * wire before a correction can land.
 *
 * Time comes from Redis rather than the caller so several api instances agree
 * on the window even when their clocks do not.
 *
 * KEYS[1]                            frozen-scope marker for this region
 * ARGV[1]                            priority: 'interactive' | 'bulk'
 * ARGV[2]                            bulk usage ceiling, 0..1 (§9.3)
 * ARGV[3]                            current interactive waiter count
 * ARGV[4]                            token unique to this acquisition
 * ARGV[5]                            number of buckets
 * ARGV[6+3i], ARGV[7+3i], ARGV[8+3i] bucket key, limit, window seconds
 *
 * Returns { 1, 0, 'ok' }             acquired
 *         { 0, wait_ms, reason }     denied; retry after wait_ms
 */
export const ACQUIRE_SCRIPT = `
local frozen_key   = KEYS[1]
local priority     = ARGV[1]
local ceiling      = tonumber(ARGV[2])
local waiters      = tonumber(ARGV[3])
local token        = ARGV[4]
local bucket_count = tonumber(ARGV[5])

local t      = redis.call('TIME')
local now_ms = tonumber(t[1]) * 1000 + math.floor(tonumber(t[2]) / 1000)

-- 1. A frozen scope beats everything: Riot told us to wait (§9.4).
local frozen_ttl = redis.call('PTTL', frozen_key)
if frozen_ttl > 0 then
  return { 0, frozen_ttl, 'frozen' }
end

-- Drop everything that has aged out, so ZCARD is the live count.
local function trim(key, window_ms)
  redis.call('ZREMRANGEBYSCORE', key, '-inf', now_ms - window_ms)
  return redis.call('ZCARD', key)
end

-- How long until the oldest admission leaves the window and frees a slot.
local function wait_for_slot(key, window_ms)
  local oldest = redis.call('ZRANGE', key, 0, 0, 'WITHSCORES')
  if oldest[2] == nil then return 1 end
  local wait = (tonumber(oldest[2]) + window_ms) - now_ms
  if wait < 1 then return 1 end
  return wait
end

-- 2. Bulk fairness (§9.3): bulk work yields to interactive traffic and refuses
--    to push any bucket past the ceiling.
if priority == 'bulk' then
  if waiters > 0 then
    return { 0, 250, 'interactive-queue-busy' }
  end
  for i = 0, bucket_count - 1 do
    local key       = ARGV[6 + i * 3]
    local limit     = tonumber(ARGV[7 + i * 3])
    local window_ms = tonumber(ARGV[8 + i * 3]) * 1000
    local used      = trim(key, window_ms)
    if limit > 0 and (used / limit) >= ceiling then
      return { 0, wait_for_slot(key, window_ms), 'bulk-ceiling' }
    end
  end
end

-- 3. Take one slot in every bucket, rolling back on the first refusal so a
--    partial acquisition can never leak slots.
local taken = {}
for i = 0, bucket_count - 1 do
  local key       = ARGV[6 + i * 3]
  local limit     = tonumber(ARGV[7 + i * 3])
  local window_ms = tonumber(ARGV[8 + i * 3]) * 1000
  local used      = trim(key, window_ms)

  if used >= limit then
    local wait = wait_for_slot(key, window_ms)
    for _, done in ipairs(taken) do
      redis.call('ZREM', done, token)
    end
    return { 0, wait, key }
  end

  redis.call('ZADD', key, now_ms, token)
  -- Outlive the window by a margin: the key is only a cache of recent
  -- admissions, and losing it early would hand out a fresh allowance.
  redis.call('PEXPIRE', key, window_ms + 5000)
  taken[#taken + 1] = key
end

return { 1, 0, 'ok' }
`;

/**
 * Reconcile our buckets with Riot's own accounting (§9.1).
 *
 * Riot reports usage per window in `X-App-Rate-Limit-Count`. Anything else
 * using the same key (a script, another deploy, a teammate's tooling) consumes
 * from the same buckets, so where Riot's count exceeds ours we pad the bucket
 * with placeholder admissions rather than trusting our own view.
 *
 * The placeholders are stamped now, not at the unknown moment Riot counted
 * them, so they age out later than the requests they stand for. That errs
 * towards holding back, which is the safe direction.
 *
 * ARGV[1]                            token unique to this sync
 * ARGV[2]                            number of buckets
 * ARGV[3+3i], ARGV[4+3i], ARGV[5+3i] bucket key, riot count, window seconds
 */
export const SYNC_SCRIPT = `
local token        = ARGV[1]
local bucket_count = tonumber(ARGV[2])

local t      = redis.call('TIME')
local now_ms = tonumber(t[1]) * 1000 + math.floor(tonumber(t[2]) / 1000)

for i = 0, bucket_count - 1 do
  local key       = ARGV[3 + i * 3]
  local count     = tonumber(ARGV[4 + i * 3])
  local window_ms = tonumber(ARGV[5 + i * 3]) * 1000

  redis.call('ZREMRANGEBYSCORE', key, '-inf', now_ms - window_ms)
  local ours = redis.call('ZCARD', key)

  if count > ours then
    -- Name placeholders after this sync's token, not the clock: two syncs in
    -- the same millisecond would otherwise pick overlapping names, and ZADD of
    -- an existing member leaves the cardinality — the whole point of the
    -- padding — short of what Riot has already counted.
    for n = ours + 1, count do
      redis.call('ZADD', key, now_ms, 'riot:' .. token .. ':' .. n)
    end
    redis.call('PEXPIRE', key, window_ms + 5000)
  end
end

return 1
`;
