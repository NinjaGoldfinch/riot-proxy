import { pino, type LoggerOptions } from 'pino';
import { config } from './config.js';

/**
 * §12.2 — the Riot key must never reach a log line, at any level. Redaction
 * covers the paths pino can see plus a censor hook for anything that slips
 * through as a raw string (e.g. a URL that somehow embedded the key).
 */
const REDACT_PATHS = [
  'req.headers.authorization',
  'req.headers["x-riot-token"]',
  'res.headers["x-riot-token"]',
  'headers.authorization',
  'headers["x-riot-token"]',
  'config.RIOT_API_KEY',
  'RIOT_API_KEY',
  'riotApiKey',
  'apiKey',
  'key',
  '*.RIOT_API_KEY',
  '*.riotApiKey',
];

/** Belt-and-braces: scrub the literal key out of any serialised string. */
export function scrubKey(input: string): string {
  if (!input) return input;
  return (
    input
      .split(config.RIOT_API_KEY)
      .join('[REDACTED]')
      // Any RGAPI token, even one we did not issue (e.g. echoed in an error body).
      .replace(/RGAPI-[0-9a-fA-F-]{8,}/g, '[REDACTED]')
      .replace(/rpx_[A-Za-z0-9_-]{8,}/g, '[REDACTED]')
  );
}

const options: LoggerOptions = {
  level: config.LOG_LEVEL,
  redact: { paths: REDACT_PATHS, censor: '[REDACTED]' },
  base: { service: 'riot-proxy' },
  formatters: {
    log(object) {
      // Recursively scrub string values so a stringified URL or error body
      // containing the key cannot leak.
      return scrubObject(object) as Record<string, unknown>;
    },
  },
};

function scrubObject(value: unknown, depth = 0): unknown {
  if (depth > 6) return value;
  if (typeof value === 'string') return scrubKey(value);
  if (Array.isArray(value)) return value.map((v) => scrubObject(v, depth + 1));
  if (value && typeof value === 'object') {
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
      out[k] = scrubObject(v, depth + 1);
    }
    return out;
  }
  return value;
}

const transport =
  !config.isProduction && !config.isTest
    ? { target: 'pino-pretty', options: { colorize: true, translateTime: 'HH:MM:ss.l' } }
    : undefined;

export const logger = pino(transport ? { ...options, transport } : options);

export type Logger = typeof logger;
