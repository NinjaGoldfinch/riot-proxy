/**
 * Lua kept inline rather than in `.lua` files: `tsc` does not copy non-TS
 * assets, and a build that silently ships without its limiter scripts is a
 * worse failure than slightly noisier source.
 */

/**
 * Atomic acquisition across every applicable Riot bucket (§9.2).
 *
 * Riot's windows start at the first request in the window, which is exactly the
 * semantics of INCR + EXPIRE-on-first-hit, so each window is one counter key.
 *
 * KEYS[1]                            frozen-scope marker for this region
 * ARGV[1]                            priority: 'interactive' | 'bulk'
 * ARGV[2]                            bulk usage ceiling, 0..1 (§9.3)
 * ARGV[3]                            current interactive waiter count
 * ARGV[4]                            number of buckets
 * ARGV[5+3i], ARGV[6+3i], ARGV[7+3i] bucket key, limit, window seconds
 *
 * Returns { 1, 0, 'ok' }             acquired
 *         { 0, wait_ms, reason }     denied; retry after wait_ms
 */
export const ACQUIRE_SCRIPT = `
local frozen_key   = KEYS[1]
local priority     = ARGV[1]
local ceiling      = tonumber(ARGV[2])
local waiters      = tonumber(ARGV[3])
local bucket_count = tonumber(ARGV[4])

-- 1. A frozen scope beats everything: Riot told us to wait (§9.4).
local frozen_ttl = redis.call('PTTL', frozen_key)
if frozen_ttl > 0 then
  return { 0, frozen_ttl, 'frozen' }
end

-- 2. Bulk fairness (§9.3): bulk work yields to interactive traffic and refuses
--    to push any bucket past the ceiling.
if priority == 'bulk' then
  if waiters > 0 then
    return { 0, 250, 'interactive-queue-busy' }
  end
  for i = 0, bucket_count - 1 do
    local key   = ARGV[5 + i * 3]
    local limit = tonumber(ARGV[6 + i * 3])
    local used  = tonumber(redis.call('GET', key) or '0')
    if limit > 0 and (used / limit) >= ceiling then
      local pttl = redis.call('PTTL', key)
      if pttl < 0 then pttl = 250 end
      return { 0, pttl, 'bulk-ceiling' }
    end
  end
end

-- 3. Take one token from every bucket, rolling back on the first refusal so a
--    partial acquisition can never leak tokens.
local taken = {}
for i = 0, bucket_count - 1 do
  local key    = ARGV[5 + i * 3]
  local limit  = tonumber(ARGV[6 + i * 3])
  local window = tonumber(ARGV[7 + i * 3])

  local used = redis.call('INCR', key)
  if used == 1 then
    redis.call('EXPIRE', key, window)
  end

  if used > limit then
    redis.call('DECR', key)
    for _, done in ipairs(taken) do
      redis.call('DECR', done)
    end
    local pttl = redis.call('PTTL', key)
    if pttl < 0 then pttl = window * 1000 end
    return { 0, pttl, key }
  end

  taken[#taken + 1] = key
end

return { 1, 0, 'ok' }
`;

/**
 * Reconcile our counters with Riot's own accounting (§9.1).
 *
 * Riot reports usage per window in `X-App-Rate-Limit-Count`. Anything else
 * using the same key (a script, another deploy, a teammate's tooling) consumes
 * from the same buckets, so we take the maximum of the two views rather than
 * trusting our local count.
 *
 * ARGV[1]                            number of buckets
 * ARGV[2+3i], ARGV[3+3i], ARGV[4+3i] bucket key, riot count, window seconds
 */
export const SYNC_SCRIPT = `
local bucket_count = tonumber(ARGV[1])

for i = 0, bucket_count - 1 do
  local key    = ARGV[2 + i * 3]
  local count  = tonumber(ARGV[3 + i * 3])
  local window = tonumber(ARGV[4 + i * 3])

  local ours = tonumber(redis.call('GET', key) or '0')
  if count > ours then
    redis.call('SET', key, count)
    local pttl = redis.call('PTTL', key)
    if pttl < 0 then
      redis.call('EXPIRE', key, window)
    end
  end
end

return 1
`;
