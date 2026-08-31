/**
 * A minimal in-memory Redis stand-in covering exactly the commands the cache,
 * single-flight and limiter layers use. Keeps the unit suite deterministic and
 * runnable with no services up; the Lua paths are covered separately against a
 * real Redis when `REDIS_URL` is reachable.
 */
export class FakeRedis {
  private store = new Map<string, { value: string; expiresAt?: number }>();
  /** Sets live beside the string store; a key is only ever one or the other. */
  private sets = new Map<string, Set<string>>();

  private live(key: string): { value: string; expiresAt?: number } | undefined {
    const entry = this.store.get(key);
    if (!entry) return undefined;
    if (entry.expiresAt !== undefined && entry.expiresAt <= Date.now()) {
      this.store.delete(key);
      return undefined;
    }
    return entry;
  }

  async get(key: string): Promise<string | null> {
    return this.live(key)?.value ?? null;
  }

  async set(key: string, value: string, ...args: unknown[]): Promise<'OK' | null> {
    const flags = args.map((a) => String(a).toUpperCase());
    const nx = flags.includes('NX');
    if (nx && this.live(key)) return null;

    let expiresAt: number | undefined;
    const exIdx = flags.indexOf('EX');
    const pxIdx = flags.indexOf('PX');
    if (exIdx !== -1) expiresAt = Date.now() + Number(args[exIdx + 1]) * 1000;
    if (pxIdx !== -1) expiresAt = Date.now() + Number(args[pxIdx + 1]);

    this.store.set(key, expiresAt === undefined ? { value } : { value, expiresAt });
    return 'OK';
  }

  async del(...keys: string[]): Promise<number> {
    let n = 0;
    for (const key of keys) {
      const had = this.store.delete(key);
      if (this.sets.delete(key) || had) n += 1;
    }
    return n;
  }

  // ── sets ───────────────────────────────────────────────────────────────────
  // The ladder crawl tracks its outstanding legs as a set, and the whole point
  // of that choice is that SREM is idempotent where DECR is not — so the fake
  // has to model membership rather than a count.

  async sadd(key: string, ...members: string[]): Promise<number> {
    const set = this.sets.get(key) ?? new Set<string>();
    const before = set.size;
    for (const member of members) set.add(member);
    this.sets.set(key, set);
    return set.size - before;
  }

  async srem(key: string, ...members: string[]): Promise<number> {
    const set = this.sets.get(key);
    if (!set) return 0;
    let removed = 0;
    for (const member of members) if (set.delete(member)) removed += 1;
    if (set.size === 0) this.sets.delete(key);
    return removed;
  }

  async scard(key: string): Promise<number> {
    return this.sets.get(key)?.size ?? 0;
  }

  async smembers(key: string): Promise<string[]> {
    return [...(this.sets.get(key) ?? [])];
  }

  async exists(key: string): Promise<number> {
    return this.live(key) || this.sets.has(key) ? 1 : 0;
  }

  async incr(key: string): Promise<number> {
    const next = Number(this.live(key)?.value ?? '0') + 1;
    const existing = this.store.get(key);
    this.store.set(key, {
      value: String(next),
      ...(existing?.expiresAt !== undefined ? { expiresAt: existing.expiresAt } : {}),
    });
    return next;
  }

  async decr(key: string): Promise<number> {
    const next = Number(this.live(key)?.value ?? '0') - 1;
    const existing = this.store.get(key);
    this.store.set(key, {
      value: String(next),
      ...(existing?.expiresAt !== undefined ? { expiresAt: existing.expiresAt } : {}),
    });
    return next;
  }

  async expire(key: string, seconds: number): Promise<number> {
    // Set expiry is not modelled: nothing under test reads it back, and a set
    // that quietly vanished mid-test would hide the bug rather than find it.
    if (this.sets.has(key)) return 1;
    const entry = this.live(key);
    if (!entry) return 0;
    this.store.set(key, { value: entry.value, expiresAt: Date.now() + seconds * 1000 });
    return 1;
  }

  async pttl(key: string): Promise<number> {
    const entry = this.live(key);
    if (!entry) return -2;
    if (entry.expiresAt === undefined) return -1;
    return Math.max(0, entry.expiresAt - Date.now());
  }

  async ttl(key: string): Promise<number> {
    const ms = await this.pttl(key);
    return ms < 0 ? ms : Math.ceil(ms / 1000);
  }

  async mget(keys: string[]): Promise<(string | null)[]> {
    return Promise.all(keys.map((k) => this.get(k)));
  }

  async scan(
    cursor: string,
    _match: string,
    pattern: string,
    _count: string,
    _n: number,
  ): Promise<[string, string[]]> {
    const regex = globToRegExp(pattern);
    const keys = [...this.store.keys()].filter((k) => regex.test(k) && this.live(k));
    return [cursor === '0' ? '0' : '0', keys];
  }

  async ping(): Promise<string> {
    return 'PONG';
  }

  /**
   * Enough of a pipeline for the callers that batch: commands are queued by
   * name and replayed against this instance, so `exec()` returns ioredis'
   * `[error, result]` pairs without every command needing its own stub.
   */
  pipeline(): FakePipeline {
    return new FakePipeline(this);
  }

  /** `multi()` is a pipeline here — nothing under test depends on atomicity. */
  multi(): FakePipeline {
    return new FakePipeline(this);
  }

  reset(): void {
    this.store.clear();
    this.sets.clear();
  }

  get size(): number {
    return this.store.size;
  }
}

type Command = (...args: never[]) => Promise<unknown>;

class FakePipeline {
  private readonly queued: [string, unknown[]][] = [];

  constructor(private readonly redis: FakeRedis) {}

  pttl(key: string): this {
    this.queued.push(['pttl', [key]]);
    return this;
  }

  sadd(key: string, ...members: string[]): this {
    this.queued.push(['sadd', [key, ...members]]);
    return this;
  }

  expire(key: string, seconds: number): this {
    this.queued.push(['expire', [key, seconds]]);
    return this;
  }

  async exec(): Promise<[Error | null, unknown][]> {
    const target = this.redis as unknown as Record<string, Command>;
    return Promise.all(
      this.queued.map(async ([name, args]): Promise<[Error | null, unknown]> => [
        null,
        await target[name]!(...(args as never[])),
      ]),
    );
  }
}

function globToRegExp(pattern: string): RegExp {
  const escaped = pattern
    .replace(/[.+^${}()|[\]\\]/g, '\\$&')
    .replace(/\*/g, '.*')
    .replace(/\?/g, '.');
  return new RegExp(`^${escaped}$`);
}
