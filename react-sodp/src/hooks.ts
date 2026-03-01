import {useEffect, useMemo, useState} from "react";
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
): [value: T | null, meta: WatchMeta | null, ref: StateRef<T> | null] {
  const client = useSodpClient();

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
    });
  }, [client, key]);

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
