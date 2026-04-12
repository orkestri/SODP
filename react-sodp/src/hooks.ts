import {useEffect, useMemo, useRef, useState} from "react";
import {type StateRef, type WatchMeta} from "@sodp/client";
import {useSodpClient} from "./context.js";

// ── useSodpState ──────────────────────────────────────────────────────────────

/**
 * Subscribe to a SODP state key and re-render on every change.
 *
 * Returns a `[value, meta, ref]` tuple:
 * - `value` — current state (typed `T`), or `null` if the key has no value yet.
 * - `meta`  — `{ version, initialized }`, or `null` before the first server update.
 * - `ref`   — `StateRef<T>` for writes (`set`, `patch`, `setIn`, `delete`).
 *             `null` while the client is connecting.
 *
 * Multiple components watching the same key share a **single server subscription**.
 * The callback fires on every DELTA and STATE_INIT, so you always see the latest value.
 *
 * @example
 * ```tsx
 * interface Player { name: string; health: number }
 *
 * function HUD() {
 *   const [player, meta, ref] = useSodpState<Player>("game.player");
 *
 *   if (!meta?.initialized) return <Spinner />;
 *
 *   return (
 *     <>
 *       <div>{player?.name} — HP: {player?.health}</div>
 *       <button onClick={() => ref?.patch({ health: (player?.health ?? 0) - 10 })}>
 *         Take damage
 *       </button>
 *     </>
 *   );
 * }
 * ```
 */
export function useSodpState<T = unknown>(
  key: string,
  params?: Record<string, unknown>,
): [value: T | null, meta: WatchMeta | null, ref: StateRef<T> | null] {
  const client = useSodpClient();

  // Memoize params by value so that inline objects don't cause re-subscriptions.
  const stableParams = useMemo(
    () => params,
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [params ? JSON.stringify(params) : undefined],
  );

  const [value, setValue] = useState<T | null>(null);
  const [meta,  setMeta]  = useState<WatchMeta | null>(null);

  // Stable ref handle — recreated only when client or key changes.
  const ref = useMemo<StateRef<T> | null>(
    () => (client ? client.state<T>(key) : null),
    [client, key],
  );

  useEffect(() => {
    if (!client) return;

    // Reset stale value when key changes so the component never briefly shows
    // a previous key's data while waiting for the new STATE_INIT.
    setValue(null);
    setMeta(null);

    return client.watch<T>(key, (v, m) => {
      setValue(v);
      setMeta(m);
    }, stableParams);
  }, [client, key, stableParams]);

  return [value, meta, ref];
}

// ── useSodpRef ────────────────────────────────────────────────────────────────

/**
 * Returns a `StateRef<T>` for write operations on a state key without
 * subscribing to its value.
 *
 * Use this when a component needs to mutate state but not re-render on
 * every change (e.g. a button that resets a counter, a form that submits).
 *
 * Returns `null` while the client is initializing.
 *
 * @example
 * ```tsx
 * function ResetButton() {
 *   const ref = useSodpRef<{ score: number }>("game.score");
 *   return <button onClick={() => ref?.set({ score: 0 })}>Reset</button>;
 * }
 * ```
 */
export function useSodpRef<T = unknown>(key: string): StateRef<T> | null {
  const client = useSodpClient();
  return useMemo<StateRef<T> | null>(
    () => (client ? client.state<T>(key) : null),
    [client, key],
  );
}

// ── useSodpStates ──────────────────────────────────────────────────────────────

/**
 * Subscribe to multiple SODP state keys in a single server frame.
 *
 * Returns a `Map` where each key maps to `[value, meta]`.  The map updates
 * and triggers a re-render whenever any of the watched keys changes.
 *
 * @example
 * ```tsx
 * function Dashboard() {
 *   const states = useSodpStates<number>(["score.a", "score.b", "score.c"]);
 *   return (
 *     <ul>
 *       {[...states].map(([key, [value, meta]]) => (
 *         <li key={key}>{key}: {value} (v{meta?.version})</li>
 *       ))}
 *     </ul>
 *   );
 * }
 * ```
 */
export function useSodpStates<T = unknown>(
  keys: string[],
  params?: Record<string, unknown>,
): Map<string, [T | null, WatchMeta | null]> {
  const client = useSodpClient();

  const stableKeys = useMemo(
    () => keys,
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [JSON.stringify(keys)],
  );

  const stableParams = useMemo(
    () => params,
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [params ? JSON.stringify(params) : undefined],
  );

  const [, forceRender] = useState(0);
  const mapRef = useRef(new Map<string, [T | null, WatchMeta | null]>());

  useEffect(() => {
    if (!client) return;

    mapRef.current = new Map(stableKeys.map(k => [k, [null, null]]));
    forceRender(n => n + 1);

    // Subscribe per-key — each callback knows exactly which key it belongs to.
    // The server receives individual WATCH frames (multi-key is used at the
    // wire level for batch reconnect; the React hook uses per-key for clarity).
    const unsubs = stableKeys.map(key =>
      client.watch<T>(key, (value, meta) => {
        mapRef.current.set(key, [value, meta]);
        forceRender(n => n + 1);
      }, stableParams),
    );

    return () => { for (const unsub of unsubs) unsub(); };
  }, [client, stableKeys, stableParams]);

  return mapRef.current;
}
