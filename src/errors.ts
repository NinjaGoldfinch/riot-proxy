/**
 * §6.1 — every error the proxy returns uses one envelope and one of a closed
 * set of codes. Upstream detail is never passed through verbatim (§12.2).
 */
export const ERROR_CODES = [
  'UNAUTHORIZED',
  'FORBIDDEN',
  'QUOTA_EXCEEDED',
  'NOT_FOUND',
  'UPSTREAM_ERROR',
  'RATE_LIMITED',
  'BAD_REGION',
  'VALIDATION',
  'INTERNAL',
] as const;

export type ErrorCode = (typeof ERROR_CODES)[number];

export interface ErrorEnvelope {
  error: {
    code: ErrorCode;
    message: string;
    retryAfter?: number;
  };
}

/**
 * The status each code returns unless a call site overrides it. Exported
 * because the reference's error table is generated from it (#73) — the table
 * used to restate all nine numbers from memory.
 */
export const DEFAULT_STATUS: Record<ErrorCode, number> = {
  UNAUTHORIZED: 401,
  FORBIDDEN: 403,
  QUOTA_EXCEEDED: 429,
  NOT_FOUND: 404,
  UPSTREAM_ERROR: 502,
  RATE_LIMITED: 503,
  BAD_REGION: 400,
  VALIDATION: 400,
  INTERNAL: 500,
};

export class ProxyError extends Error {
  readonly code: ErrorCode;
  readonly statusCode: number;
  /** Seconds; surfaced both in the envelope and as a `Retry-After` header. */
  readonly retryAfter?: number;

  constructor(
    code: ErrorCode,
    message: string,
    opts: { statusCode?: number; retryAfter?: number; cause?: unknown } = {},
  ) {
    super(message, opts.cause !== undefined ? { cause: opts.cause } : undefined);
    this.name = 'ProxyError';
    this.code = code;
    this.statusCode = opts.statusCode ?? DEFAULT_STATUS[code];
    if (opts.retryAfter !== undefined) this.retryAfter = opts.retryAfter;
  }

  toEnvelope(): ErrorEnvelope {
    const error: ErrorEnvelope['error'] = { code: this.code, message: this.message };
    if (this.retryAfter !== undefined) error.retryAfter = this.retryAfter;
    return { error };
  }

  static notFound(message = 'Resource not found') {
    return new ProxyError('NOT_FOUND', message);
  }
  static unauthorized(message = 'Missing or invalid API key') {
    return new ProxyError('UNAUTHORIZED', message);
  }
  static badRegion(message: string) {
    return new ProxyError('BAD_REGION', message);
  }
  static rateLimited(retryAfterSeconds: number, message = 'Upstream rate limit budget exceeded') {
    return new ProxyError('RATE_LIMITED', message, { retryAfter: retryAfterSeconds });
  }
  static upstream(message = 'Upstream request failed') {
    return new ProxyError('UPSTREAM_ERROR', message);
  }
}

/**
 * A typed upstream failure. Deliberately separate from ProxyError: this one
 * carries Riot's raw status for internal policy decisions (§5.5) and is
 * translated into a sanitised ProxyError at the route boundary.
 */
export class RiotError extends Error {
  readonly status: number;
  readonly method: string;
  readonly host: string;
  readonly retryAfter?: number;
  readonly rateLimitType?: string;

  constructor(
    status: number,
    method: string,
    host: string,
    message: string,
    extra: { retryAfter?: number; rateLimitType?: string } = {},
  ) {
    super(message);
    this.name = 'RiotError';
    this.status = status;
    this.method = method;
    this.host = host;
    if (extra.retryAfter !== undefined) this.retryAfter = extra.retryAfter;
    if (extra.rateLimitType !== undefined) this.rateLimitType = extra.rateLimitType;
  }

  get isNotFound() {
    return this.status === 404;
  }
  get isAuthFailure() {
    return this.status === 401 || this.status === 403;
  }
  get isRateLimited() {
    return this.status === 429;
  }
  get isServerError() {
    return this.status >= 500;
  }

  /** §5.5 — map an upstream status onto the client-facing envelope. */
  toProxyError(): ProxyError {
    if (this.isNotFound) return ProxyError.notFound('Resource not found upstream');
    if (this.isAuthFailure) {
      // Never tell the client the key is bad — that is an operator problem.
      return ProxyError.upstream('Upstream authentication failed');
    }
    if (this.isRateLimited) {
      return ProxyError.rateLimited(this.retryAfter ?? 1, 'Upstream rate limited');
    }
    return ProxyError.upstream('Upstream request failed');
  }
}
