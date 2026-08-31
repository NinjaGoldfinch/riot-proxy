import type { FastifyPluginAsync } from 'fastify';
// Importing the plugin's types is what teaches Fastify about `websocket: true`
// and the (socket, request) handler signature.
import type { WebSocket } from '@fastify/websocket';
import '@fastify/websocket';
import { resolveConsumer, bearerFrom, ipAllowed, DEV_CONSUMER } from '../auth/plugin.js';
import { config } from '../config.js';
import {
  ADMIN_TOPICS,
  CHANNEL_PATTERN,
  FIREHOSE_TOPIC,
  parseEvent,
  topicFromChannel,
} from '../events/index.js';
import { logger } from '../logger.js';
import { wsConnections } from '../metrics.js';
import { subscriber } from '../redis.js';
import { HEARTBEAT_MS, MAX_MISSED_PONGS, MAX_TOPICS_PER_SOCKET } from './constants.js';

interface Client {
  socket: WebSocket;
  topics: Set<string>;
  missedPongs: number;
  consumer: string;
  /**
   * Resolved once at the handshake — scope *and* the admin IP allowlist, the
   * same pair the HTTP admin routes check — so the subscribe handler can gate
   * `ADMIN_TOPICS` without another round-trip.
   */
  admin: boolean;
}

/**
 * One Redis pattern-subscription per api instance relays to that instance's
 * local sockets (§11). Topic filtering is done in-process — pattern subscribe
 * is cheaper than churning SUBSCRIBE/UNSUBSCRIBE as clients come and go.
 */
export class WsHub {
  private readonly clients = new Set<Client>();
  private started = false;
  private heartbeat?: NodeJS.Timeout;

  /** Events relayed since this instance started, by event name (cumulative). */
  private readonly relayed = new Map<string, number>();

  get size(): number {
    return this.clients.size;
  }

  /** Topics held, summed over all local sockets. */
  get subscriptionCount(): number {
    let total = 0;
    for (const client of this.clients) total += client.topics.size;
    return total;
  }

  get eventCounts(): Record<string, number> {
    return Object.fromEntries(this.relayed);
  }

  countSubscribers(topic: string): number {
    let total = 0;
    for (const client of this.clients) if (client.topics.has(topic)) total += 1;
    return total;
  }

  async start(): Promise<void> {
    if (this.started) return;
    this.started = true;

    const sub = subscriber();
    await sub.psubscribe(CHANNEL_PATTERN);
    sub.on('pmessage', (_pattern: string, channel: string, message: string) => {
      this.relay(topicFromChannel(channel), message);
    });

    this.heartbeat = setInterval(() => this.pingAll(), HEARTBEAT_MS);
    this.heartbeat.unref();
    logger.info('websocket hub started');
  }

  async stop(): Promise<void> {
    if (this.heartbeat) clearInterval(this.heartbeat);
    for (const client of this.clients) client.socket.close(1001, 'server shutting down');
    this.clients.clear();
    wsConnections.set(0);
    this.started = false;
  }

  add(client: Client): void {
    this.clients.add(client);
    wsConnections.set(this.clients.size);
  }

  remove(client: Client): void {
    this.clients.delete(client);
    wsConnections.set(this.clients.size);
  }

  private relay(topic: string, message: string): void {
    const event = parseEvent(message);
    if (!event) return;
    this.relayed.set(event.event, (this.relayed.get(event.event) ?? 0) + 1);
    for (const client of this.clients) {
      // The firehose mirrors every topic. No authz check here: only an admin
      // socket can ever hold it (see the subscribe handler).
      if (!client.topics.has(topic) && !client.topics.has(FIREHOSE_TOPIC)) continue;
      safeSend(client.socket, message);
    }
  }

  private pingAll(): void {
    for (const client of this.clients) {
      if (client.missedPongs >= MAX_MISSED_PONGS) {
        logger.debug({ consumer: client.consumer }, 'dropping unresponsive websocket');
        client.socket.terminate();
        this.remove(client);
        continue;
      }
      client.missedPongs += 1;
      try {
        client.socket.ping();
      } catch {
        this.remove(client);
      }
    }
  }
}

export const wsHub = new WsHub();

function safeSend(socket: WebSocket, data: string): void {
  try {
    if (socket.readyState === socket.OPEN) socket.send(data);
  } catch (err) {
    logger.debug({ err }, 'websocket send failed');
  }
}

interface ClientMessage {
  op?: string;
  topics?: unknown;
}

/** §11 — the subscribe protocol. */
function onMessage(client: Client, raw: string): void {
  const { socket } = client;
  let msg: ClientMessage;
  try {
    msg = JSON.parse(raw) as ClientMessage;
  } catch {
    safeSend(
      socket,
      JSON.stringify({ op: 'error', error: { code: 'VALIDATION', message: 'Invalid JSON' } }),
    );
    return;
  }

  const topics = Array.isArray(msg.topics)
    ? msg.topics.filter((t): t is string => typeof t === 'string' && t.length <= 200)
    : [];

  switch (msg.op) {
    case 'subscribe': {
      for (const topic of topics) {
        if (ADMIN_TOPICS.has(topic) && !client.admin) {
          safeSend(
            socket,
            JSON.stringify({
              op: 'error',
              error: { code: 'FORBIDDEN', message: `Topic '${topic}' requires the admin scope` },
            }),
          );
          continue;
        }
        if (client.topics.size >= MAX_TOPICS_PER_SOCKET) break;
        client.topics.add(topic);
      }
      // The acknowledgement lists what the socket actually holds, so a refused
      // or dropped topic is visible the same way: absent.
      safeSend(socket, JSON.stringify({ op: 'subscribed', topics: [...client.topics] }));
      break;
    }
    case 'unsubscribe': {
      for (const topic of topics) client.topics.delete(topic);
      safeSend(socket, JSON.stringify({ op: 'subscribed', topics: [...client.topics] }));
      break;
    }
    case 'ping':
      safeSend(socket, JSON.stringify({ op: 'pong', at: Date.now() }));
      break;
    default:
      safeSend(
        socket,
        JSON.stringify({
          op: 'error',
          error: { code: 'VALIDATION', message: `Unknown op '${String(msg.op)}'` },
        }),
      );
  }
}

const wsRoutes: FastifyPluginAsync = async (fastify) => {
  await wsHub.start();

  fastify.get(
    '/v1/ws',
    // The handshake is authenticated here rather than by the global hook: a
    // browser cannot set headers on a WebSocket, so `?token=` must be allowed.
    { websocket: true, config: { public: true } },
    async (socket, request) => {
      /**
       * Authenticating means awaiting Redis/Postgres, and a client that sends
       * `subscribe` the instant its socket opens would have that frame dropped
       * — `ws` discards messages arriving before a listener exists. So buffer
       * synchronously here, then drain once the consumer is known.
       */
      const pending: string[] = [];
      let handle: (raw: string) => void = (raw) => {
        if (pending.length < 32) pending.push(raw);
      };
      socket.on('message', (raw: Buffer) => handle(raw.toString()));

      const token = bearerFrom(request);
      const consumer = config.authDisabled
        ? DEV_CONSUMER
        : token
          ? await resolveConsumer(token)
          : undefined;

      if (!consumer) {
        socket.send(
          JSON.stringify({ op: 'error', error: { code: 'UNAUTHORIZED', message: 'Invalid key' } }),
        );
        socket.close(4401, 'unauthorized');
        return;
      }

      const client: Client = {
        socket,
        topics: new Set<string>(),
        missedPongs: 0,
        consumer: consumer.name,
        admin: consumer.scopes.includes('admin') && ipAllowed(request.ip, config.adminIpAllowlist),
      };
      wsHub.add(client);
      logger.debug({ consumer: consumer.name }, 'websocket connected');

      socket.on('pong', () => {
        client.missedPongs = 0;
      });

      handle = (raw: string) => onMessage(client, raw);
      for (const buffered of pending.splice(0)) handle(buffered);

      socket.on('close', () => {
        wsHub.remove(client);
        logger.debug({ consumer: consumer.name }, 'websocket closed');
      });

      socket.on('error', (err: Error) => {
        logger.debug({ err }, 'websocket error');
        wsHub.remove(client);
      });

      safeSend(socket, JSON.stringify({ op: 'ready', consumer: consumer.name }));
    },
  );
};

export default wsRoutes;
