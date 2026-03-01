import { createContext, useContext, useEffect, useState } from "react";
import type { ReactNode } from "react";
import { SodpClient, type SodpClientOptions } from "@sodp/client";

// ── Context ───────────────────────────────────────────────────────────────────

export interface SODPContextValue {
  /** The active client, or `null` while the connection is being initialized. */
  client: SodpClient | null;
  /** `true` once the client is authenticated and ready to receive data. */
  connected: boolean;
}

/** @internal */
export const SODPContext = createContext<SODPContextValue>({
  client: null,
  connected: false,
});

// ── Provider ──────────────────────────────────────────────────────────────────

export interface SODPProviderProps
  extends Omit<SodpClientOptions, "onConnect" | "onDisconnect"> {
  /** WebSocket server URL, e.g. `"ws://localhost:7777"` or `"wss://sodp.example.com"`. */
  url: string;
  children: ReactNode;
  /** Called each time the connection is established and authenticated. */
  onConnect?: () => void;
  /** Called each time the connection drops (before any reconnect attempt). */
  onDisconnect?: () => void;
}

/**
 * Provides a SODP client to the React component tree.
 *
 * Place near the root of your app — all `useSodpState` and `useSodpRef` calls
 * inside must be descendants of this provider.
 *
 * The client is created once on mount and closed on unmount.  If `url`
 * changes the old client is closed and a new one is opened.  Other options
 * (token, reconnect settings) are read once on mount; to rotate a token
 * cleanly, unmount and remount the provider with the new token.
 *
 * @example
 * ```tsx
 * <SODPProvider url="ws://localhost:7777" token={jwt}>
 *   <App />
 * </SODPProvider>
 * ```
 */
export function SODPProvider({
  url,
  children,
  onConnect,
  onDisconnect,
  ...opts
}: SODPProviderProps) {
  const [ctx, setCtx] = useState<SODPContextValue>({ client: null, connected: false });

  useEffect(() => {
    const client = new SodpClient(url, {
      ...opts,
      onConnect:    () => { onConnect?.();    setCtx(c => ({ ...c, connected: true  })); },
      onDisconnect: () => { onDisconnect?.(); setCtx(c => ({ ...c, connected: false })); },
    });

    setCtx({ client, connected: false });

    return () => {
      client.close();
      setCtx({ client: null, connected: false });
    };
  // opts/callbacks are intentionally excluded from deps.
  // The client is tied to the URL — changing anything else requires remounting.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [url]);

  return <SODPContext.Provider value={ctx}>{children}</SODPContext.Provider>;
}

// ── Escape hatches ────────────────────────────────────────────────────────────

/**
 * Returns the raw `SodpClient` from the nearest `SODPProvider`.
 *
 * Returns `null` during the brief window between mount and the first WebSocket
 * connection.  For most use cases, prefer `useSodpState` or `useSodpRef`.
 */
export function useSodpClient(): SodpClient | null {
  return useContext(SODPContext).client;
}

/**
 * Returns `true` once the client is authenticated and ready.
 * Useful for showing a connection status indicator.
 *
 * @example
 * ```tsx
 * const connected = useSodpConnected();
 * return <span>{connected ? "Live" : "Connecting…"}</span>;
 * ```
 */
export function useSodpConnected(): boolean {
  return useContext(SODPContext).connected;
}
