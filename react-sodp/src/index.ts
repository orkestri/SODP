export { SODPProvider, useSodpClient, useSodpConnected } from "./context.js";
export type { SODPProviderProps, SODPContextValue } from "./context.js";
export { useSodpState, useSodpStates, useSodpRef } from "./hooks.js";

// Re-export core types and helpers from @sodp/client so React apps only need
// one import path. Includes the `source` field on WatchMeta (cache | init | delta)
// and the shared `applyOps` reducer for test authors.
export { SodpClient, StateRef, applyOps } from "@sodp/client";
export type { WatchMeta, WatchCallback, DeltaOp, SodpClientOptions, CallResult } from "@sodp/client";
