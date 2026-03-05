import { encode, decode } from "@msgpack/msgpack";
import { applyOps, type DeltaOp } from "./delta.js";

// ── Frame type constants ───────────────────────────────────────────────────────

const HELLO      = 0x01;
const WATCH      = 0x02;
const STATE_INIT = 0x03;
const DELTA      = 0x04;
const CALL       = 0x05;
const RESULT     = 0x06;
const ERROR      = 0x07;
const HEARTBEAT  = 0x09;
const RESUME     = 0x0a;
const AUTH       = 0x0b;
const AUTH_OK    = 0x0c;
const UNWATCH    = 0x0d;

// ── Public types ───────────────────────────────────────────────────────────────

export interface WatchMeta {
  version:     number;
  /** false when the key has never been written to the server. */
  initialized: boolean;
}

export type WatchCallback<T> = (value: T | null, meta: WatchMeta) => void;

export interface SodpClientOptions {
  /** Static JWT token.  Used when no `tokenProvider` is given. */
  token?: string;
  /**
   * Called on every connect/reconnect to obtain a fresh JWT.
   * Supersedes `token` when both are supplied.
   *
   * May return a string or a Promise<string>, making it easy to call
   * your backend's token-refresh endpoint:
   *
   * @example
   * new SodpClient(url, {
   *   tokenProvider: async () => {
   *     const res = await fetch("/api/sodp-token");
   *     return res.text();
   *   },
   * });
   */
  tokenProvider?: () => string | Promise<string>;
  /**
   * Custom WebSocket constructor.  Pass the `ws` package class when running
   * in Node.js < 21 (which lacks a native WebSocket implementation).
   *
   * @example
   * import WebSocket from "ws";
   * new SodpClient(url, { WebSocket });
   */
  WebSocket?: typeof globalThis.WebSocket;
  /** Enable auto-reconnect (default: true). */
  reconnect?: boolean;
  /** Base reconnect delay in ms (default: 1000). Uses exponential backoff. */
  reconnectDelay?: number;
  /** Maximum reconnect delay in ms (default: 30 000). */
  maxReconnectDelay?: number;
  /** Called each time the connection is established and authenticated. */
  onConnect?: () => void;
  /** Called each time the connection drops (before any reconnect attempt). */
  onDisconnect?: () => void;
}

export interface CallResult<D = unknown> {
  version?: number;
  [key: string]: D | number | undefined;
}

// ── Internal types ─────────────────────────────────────────────────────────────

interface WatchEntry {
  key:         string;
  callbacks:   Set<WatchCallback<unknown>>;
  version:     number;
  value:       unknown;
  initialized: boolean;
}

interface PendingCall {
  resolve: (data: unknown) => void;
  reject:  (err: Error)    => void;
  timer:   ReturnType<typeof setTimeout>;
}

type WireFrame = [number, number, number, unknown];

// ── SodpClient ─────────────────────────────────────────────────────────────────

/**
 * Client for the State-Oriented Data Protocol.
 *
 * @example
 * const client = new SodpClient("ws://localhost:7777", { token: myJwt });
 *
 * // Subscribe to a state key
 * const unsub = client.watch<{ score: number }>("game.score", (value, meta) => {
 *   console.log("score:", value?.score, "version:", meta.version);
 * });
 *
 * // Mutate state
 * await client.call("state.set", { state: "game.score", value: { score: 42 } });
 *
 * // Cleanup
 * unsub();
 * client.close();
 */
export class SodpClient {
  private readonly url:  string;
  private readonly opts: Required<Omit<SodpClientOptions, "token" | "tokenProvider" | "onConnect" | "onDisconnect">> & { token?: string; tokenProvider?: () => string | Promise<string>; onConnect?: () => void; onDisconnect?: () => void };

  private ws:               WebSocket | null = null;
  private authenticated:    boolean          = false;
  private closed:           boolean          = false;
  private reconnectAttempts = 0;
  private reconnectTimer:   ReturnType<typeof setTimeout> | null = null;

  private _readyResolve: (() => void) | null = null;
  /** Resolves the first time the client successfully connects and authenticates. */
  readonly ready: Promise<void>;

  /** key → watch state */
  private readonly watches: Map<string, WatchEntry> = new Map();
  /** stream_id → state key (current connection only) */
  private streamToKey: Map<number, string> = new Map();
  /** call_id → pending promise */
  private readonly pendingCalls: Map<string, PendingCall> = new Map();

  private nextStreamId = 10;
  private nextSeq      = 0;

  /** Frames queued while not yet authenticated; flushed in onReady(). */
  private readonly callQueue: Array<[number, number, unknown]> = [];

  constructor(url: string, opts: SodpClientOptions = {}) {
    this.url  = url;
    this.opts = {
      token:              opts.token,
      tokenProvider:      opts.tokenProvider,
      onConnect:          opts.onConnect,
      onDisconnect:       opts.onDisconnect,
      WebSocket:          opts.WebSocket ?? (globalThis as unknown as { WebSocket: typeof globalThis.WebSocket }).WebSocket,
      reconnect:          opts.reconnect          ?? true,
      reconnectDelay:     opts.reconnectDelay     ?? 1_000,
      maxReconnectDelay:  opts.maxReconnectDelay  ?? 30_000,
    };
    this.ready = new Promise<void>(resolve => { this._readyResolve = resolve; });
    this.connect();
  }

  // ── Connection management ────────────────────────────────────────────────────

  private connect(): void {
    if (this.closed) return;

    const WS = this.opts.WebSocket;
    if (!WS) {
      throw new Error(
        "[SODP] No WebSocket implementation found. " +
        "Pass `{ WebSocket }` from the `ws` npm package in Node.js < 21."
      );
    }

    const ws = new WS(this.url) as WebSocket;
    ws.binaryType = "arraybuffer";
    this.ws = ws;

    ws.onmessage = (event: MessageEvent<ArrayBuffer>) => {
      try {
        this.handleFrame(decode(new Uint8Array(event.data)) as WireFrame);
      } catch (e) {
        console.error("[SODP] frame decode error:", e);
      }
    };

    ws.onclose = () => this.onDisconnect();
    ws.onerror = (e: Event) => console.warn("[SODP] WebSocket error:", e);
    // onopen is not needed — we wait for HELLO from the server.
  }

  private onDisconnect(): void {
    this.ws            = null;
    this.authenticated = false;
    this.streamToKey   = new Map();
    this.nextStreamId  = 10;
    this.nextSeq       = 0;

    // Reject all pending calls — the server won't respond to them anymore.
    for (const [, pending] of this.pendingCalls) {
      clearTimeout(pending.timer);
      pending.reject(new Error("[SODP] connection lost"));
    }
    this.pendingCalls.clear();
    this.callQueue.length = 0;

    this.opts.onDisconnect?.();

    if (this.closed || !this.opts.reconnect) return;

    const base  = this.opts.reconnectDelay;
    const max   = this.opts.maxReconnectDelay;
    const delay = Math.min(base * 2 ** this.reconnectAttempts + Math.random() * 500, max);
    this.reconnectAttempts++;

    console.debug(`[SODP] reconnecting in ${Math.round(delay)} ms (attempt ${this.reconnectAttempts})`);
    this.reconnectTimer = setTimeout(() => this.connect(), delay);
  }

  // ── Frame handling ───────────────────────────────────────────────────────────

  private handleFrame([type, streamId, , body]: WireFrame): void {
    switch (type) {
      case HELLO:      void this.onHello(body as { auth: boolean; version: string });    break;
      case AUTH_OK:    this.onAuthOk(body as { sub: string });                           break;
      case STATE_INIT: this.onStateInit(streamId, body as { state: string; version: number; value: unknown; initialized: boolean }); break;
      case DELTA:      this.onDelta(streamId, body as { version: number; ops: DeltaOp[] }); break;
      case RESULT:     this.onResult(body as { call_id: string; success: boolean; data: unknown }); break;
      case ERROR:      this.onError(body as { code: number; message: string });          break;
      case HEARTBEAT:  this.send(HEARTBEAT, 0, null);                                   break;
    }
  }

  private async onHello(body: { auth: boolean }): Promise<void> {
    if (body.auth) {
      let token: string | undefined;
      if (this.opts.tokenProvider) {
        token = await Promise.resolve(this.opts.tokenProvider());
      } else {
        token = this.opts.token;
      }
      if (!token) {
        console.error("[SODP] server requires auth but no token or tokenProvider was supplied");
        this.ws?.close();
        return;
      }
      this.send(AUTH, 0, { token });
    } else {
      // No auth required — go live immediately.
      this.onReady();
    }
  }

  private onAuthOk(body: { sub: string }): void {
    console.debug(`[SODP] authenticated as "${body.sub}"`);
    this.onReady();
  }

  private onReady(): void {
    this.authenticated    = true;
    this.reconnectAttempts = 0;

    // Re-subscribe after reconnect.  Keys with a known version use RESUME so
    // the server replays any missed deltas before sending STATE_INIT.
    for (const [key, entry] of this.watches) {
      if (entry.callbacks.size === 0) continue;
      if (entry.version > 0) {
        this.sendResume(key, entry.version);
      } else {
        this.sendWatch(key);
      }
    }

    // Flush calls that were queued before the connection was ready.
    for (const [type, streamId, body] of this.callQueue) {
      this.send(type, streamId, body);
    }
    this.callQueue.length = 0;

    this._readyResolve?.();
    this._readyResolve = null;
    this.opts.onConnect?.();
  }

  private onStateInit(streamId: number, body: { state: string; version: number; value: unknown; initialized: boolean }): void {
    // Route by the key included in the body — reliable regardless of stream_id
    // allocation order.  Also update streamToKey so incoming DELTAs (which only
    // carry stream_id, not the key) are routed correctly.
    const key   = body.state;
    const entry = this.watches.get(key);
    if (!entry) return;

    this.streamToKey.set(streamId, key);  // bind server-assigned stream_id → key

    entry.version     = body.version;
    entry.value       = body.value;
    entry.initialized = body.initialized;

    const meta = { version: body.version, initialized: body.initialized };
    for (const cb of entry.callbacks) cb(body.value, meta);
  }

  private onDelta(streamId: number, body: { version: number; ops: DeltaOp[] }): void {
    const key   = this.streamToKey.get(streamId);
    const entry = key ? this.watches.get(key) : undefined;
    if (!entry) return;

    entry.value   = applyOps(entry.value, body.ops);
    entry.version = body.version;
    // A REMOVE op at the root means the key was deleted server-side —
    // treat it as uninitialized again so callers can distinguish null (no value)
    // from deleted (key gone entirely).
    const isDeleted = body.ops.some(op => op.op === "REMOVE" && op.path === "/");
    entry.initialized = !isDeleted;

    const meta = { version: body.version, initialized: entry.initialized };
    for (const cb of entry.callbacks) cb(entry.value, meta);
  }

  private onResult(body: { call_id: string; success: boolean; data: unknown }): void {
    const pending = this.pendingCalls.get(body.call_id);
    if (!pending) return;
    clearTimeout(pending.timer);
    this.pendingCalls.delete(body.call_id);

    if (body.success) {
      pending.resolve(body.data);
    } else {
      pending.reject(new Error(`[SODP] call failed: ${JSON.stringify(body.data)}`));
    }
  }

  private onError(body: { code: number; message: string }): void {
    const msg = `[SODP] ERROR ${body.code}: ${body.message}`;
    console.warn(msg);

    // An error on the control stream rejects the oldest pending call.
    // (The protocol currently has no call_id in ERROR frames.)
    const first = this.pendingCalls.entries().next();
    if (!first.done) {
      const [callId, pending] = first.value;
      clearTimeout(pending.timer);
      this.pendingCalls.delete(callId);
      pending.reject(new Error(msg));
    }
  }

  // ── Sending helpers ──────────────────────────────────────────────────────────

  private send(type: number, streamId: number, body: unknown): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return;
    this.ws.send(encode([type, streamId, ++this.nextSeq, body]));
  }

  /** Send immediately if authenticated, otherwise buffer for the next onReady(). */
  private sendOrQueue(type: number, streamId: number, body: unknown): void {
    if (this.authenticated) {
      this.send(type, streamId, body);
    } else {
      this.callQueue.push([type, streamId, body]);
    }
  }

  private sendWatch(key: string): void {
    // stream_id is arbitrary here — server will allocate its own and echo it
    // back in STATE_INIT body.state, which is where we populate streamToKey.
    this.send(WATCH, this.nextStreamId++, { state: key });
  }

  private sendResume(key: string, sinceVersion: number): void {
    // Same — server echoes the real stream_id in STATE_INIT, not here.
    this.send(RESUME, this.nextStreamId++, { state: key, since_version: sinceVersion });
  }

  // ── Public API ───────────────────────────────────────────────────────────────

  /**
   * Subscribe to a state key.
   *
   * The callback fires immediately with the current cached value (if any),
   * then on every server-side update.  Multiple calls with the same key share
   * a single server subscription.
   *
   * Returns an unsubscribe function.
   */
  watch<T = unknown>(key: string, callback: WatchCallback<T>): () => void {
    const cb = callback as WatchCallback<unknown>;
    let entry = this.watches.get(key);

    if (!entry) {
      entry = { key, callbacks: new Set(), version: 0, value: null, initialized: false };
      this.watches.set(key, entry);
      if (this.authenticated) this.sendWatch(key);
    } else if (entry.initialized) {
      // Fire immediately only when we have a confirmed value from the server.
      // If still waiting for STATE_INIT, the callback will fire when it arrives.
      cb(entry.value, { version: entry.version, initialized: entry.initialized });
    }

    entry.callbacks.add(cb);

    return () => {
      this.watches.get(key)?.callbacks.delete(cb);
    };
  }

  /**
   * Call a server method.
   *
   * Built-in methods:
   * - `"state.set"` — replace the full value of a state key
   *   `{ state: "key", value: <any> }`
   * - `"state.patch"` — shallow-merge a partial object into existing state
   *   `{ state: "key", patch: { field: value, ... } }`
   *
   * Returns the `data` field from the RESULT frame.
   */
  call<D = CallResult>(method: string, args: Record<string, unknown>): Promise<D> {
    return new Promise<D>((resolve, reject) => {
      const callId = crypto.randomUUID();

      const timer = setTimeout(() => {
        if (this.pendingCalls.delete(callId)) {
          reject(new Error(`[SODP] call timeout: ${method}`));
        }
      }, 30_000);

      this.pendingCalls.set(callId, {
        resolve: resolve as (v: unknown) => void,
        reject,
        timer,
      });

      this.sendOrQueue(CALL, 0, { call_id: callId, method, args });
    });
  }

  /** Replace the full value of a state key. */
  set(key: string, value: unknown): Promise<CallResult> {
    return this.call("state.set", { state: key, value });
  }

  /** Deep-merge a partial object into existing state. Objects are merged recursively; arrays and scalars are replaced. */
  patch(key: string, partial: Record<string, unknown>): Promise<CallResult> {
    return this.call("state.patch", { state: key, patch: partial });
  }

  /**
   * Set a nested path within a state key and bind it to this session's lifetime.
   * When this client disconnects for any reason the server automatically removes
   * the path and broadcasts the resulting DELTA to all watchers — eliminating
   * ghost presence entries.
   *
   * @example
   * // Register cursor; removed from collab.cursors when tab closes or network drops.
   * await client.presence("collab.cursors", "/alice", { name: "Alice", line: 1 });
   */
  presence(key: string, path: string, value: unknown): Promise<CallResult> {
    return this.call("state.presence", { state: key, path, value });
  }

  /**
   * Gracefully close the connection and stop reconnecting.
   */
  close(): void {
    this.closed = true;
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.ws?.close();
  }

  /**
   * Current cached value for a watched key.
   *
   * - Returns `null`  when the key is watched but has no value on the server yet.
   * - Returns `undefined` when the key is not being watched at all.
   *
   * Use `isWatching(key)` to distinguish the two null-ish states without
   * relying on the `undefined` sentinel.
   */
  getSnapshot<T = unknown>(key: string): T | null | undefined {
    const entry = this.watches.get(key);
    return entry ? (entry.value as T | null) : undefined;
  }

  /** Returns `true` if this client has an active subscription for `key`. */
  isWatching(key: string): boolean {
    return this.watches.has(key);
  }

  /**
   * Cancel the server subscription for `key` and clear all local state.
   *
   * All callbacks registered via `watch()` are discarded.  After this call,
   * `isWatching(key)` returns `false` and `getSnapshot(key)` returns `undefined`.
   *
   * Note: this is different from calling the unsubscribe function returned by
   * `watch()`, which only removes *one* callback while keeping the subscription
   * alive.  Use `unwatch` when you are done with the key entirely.
   */
  unwatch(key: string): void {
    if (!this.watches.has(key)) return;

    // Tell the server to stop sending updates — only if the subscription was
    // actually sent (i.e. we're currently authenticated).
    if (this.authenticated) {
      this.send(UNWATCH, 0, { state: key });
    }

    // Remove local state regardless of connection status.
    this.watches.delete(key);
    for (const [sid, k] of this.streamToKey) {
      if (k === key) { this.streamToKey.delete(sid); break; }
    }
  }

  /**
   * Returns a typed reference to a single state key.
   *
   * All operations (`watch`, `set`, `patch`, `setIn`, `delete`, `get`) are
   * scoped to this key.  The reference is lightweight — create as many as you
   * need and pass them around like handles.
   *
   * @example
   * interface Player { name: string; health: number; position: { x: number; y: number } }
   *
   * const player = client.state<Player>("game.player");
   *
   * const unsub = player.watch(value => console.log(value?.name));
   * await player.set({ name: "Alice", health: 100, position: { x: 0, y: 0 } });
   * await player.patch({ health: 80 });
   * await player.setIn("/position/x", 5);
   * await player.delete();
   */
  state<T = unknown>(key: string): StateRef<T> {
    return new StateRef<T>(this, key);
  }
}

// ── StateRef ───────────────────────────────────────────────────────────────────

/**
 * A typed, key-scoped handle for reading and writing a single state key.
 * Obtain via `client.state<T>("my.key")`.
 */
export class StateRef<T = unknown> {
  constructor(
    private readonly client: SodpClient,
    /** The state key this ref is bound to. */
    readonly key: string,
  ) {}

  /**
   * Subscribe to changes on this key.
   * Fires immediately with the cached value if the key is already known.
   * Returns an unsubscribe function.
   */
  watch(callback: WatchCallback<T>): () => void {
    return this.client.watch<T>(this.key, callback);
  }

  /**
   * Current cached value.
   * - `T | null` when being watched (`null` = key has no value on server)
   * - `undefined` when not being watched at all
   */
  get(): T | null | undefined {
    return this.client.getSnapshot<T>(this.key);
  }

  /** Returns `true` if this key is currently being watched. */
  isWatching(): boolean {
    return this.client.isWatching(this.key);
  }

  /**
   * Cancel the server subscription and clear all local state for this key.
   *
   * After calling this, `isWatching()` returns `false`, `get()` returns
   * `undefined`, and no further callbacks will fire.  The server stops
   * sending DELTAs for this key to this client.
   */
  unwatch(): void {
    this.client.unwatch(this.key);
  }

  /** Replace the full value of this key. */
  set(value: T): Promise<CallResult> {
    return this.client.set(this.key, value as unknown);
  }

  /**
   * Deep-merge a partial object into existing state.
   * Objects are merged recursively; any other type (array, scalar) is replaced.
   *
   * @example
   * // Only updates health — position, name, etc. are untouched.
   * await player.patch({ health: 80 });
   *
   * // Updates only position.x — position.y is preserved.
   * await player.patch({ position: { x: 5 } });
   */
  patch(partial: Partial<T> & Record<string, unknown>): Promise<CallResult> {
    return this.client.patch(this.key, partial as Record<string, unknown>);
  }

  /**
   * Atomically set a single nested field by JSON-pointer path.
   * No read-modify-write needed — the server applies the change atomically
   * and broadcasts only the minimal delta.
   *
   * @example
   * await player.setIn("/position/x", 5);
   * await player.setIn("/inventory/0", "new-sword");
   */
  setIn(path: string, value: unknown): Promise<CallResult> {
    return this.client.call("state.set_in", { state: this.key, path, value });
  }

  /**
   * Remove this key from the server entirely.
   * Watchers receive a final callback with `value = null` and `meta.initialized = false`.
   */
  delete(): Promise<CallResult> {
    return this.client.call("state.delete", { state: this.key });
  }

  /**
   * Set a nested path within this key and bind it to the session's lifetime.
   * The path is auto-removed when the client disconnects for any reason.
   *
   * @example
   * await cursors.presence("/alice", { name: "Alice", line: 1, col: 5 });
   */
  presence(path: string, value: unknown): Promise<CallResult> {
    return this.client.presence(this.key, path, value);
  }
}
