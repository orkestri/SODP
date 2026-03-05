import { renderHook, act } from "@testing-library/react";
import React from "react";
import type { ReactNode } from "react";
import { SODPProvider, useSodpState, useSodpRef, useSodpConnected, useSodpClient } from "./index.js";

// ── Mock SodpClient ────────────────────────────────────────────────────────────

type WatchCallback = (value: unknown, meta: { version: number; initialized: boolean }) => void;

class MockSodpClient {
  private watchers = new Map<string, Set<WatchCallback>>();
  onConnect?: () => void;
  onDisconnect?: () => void;

  constructor(_url: string, opts: Record<string, unknown> = {}) {
    this.onConnect    = opts.onConnect    as (() => void) | undefined;
    this.onDisconnect = opts.onDisconnect as (() => void) | undefined;
    // Simulate immediate connection
    setTimeout(() => this.onConnect?.(), 0);
  }

  watch<T>(key: string, cb: WatchCallback): () => void {
    let set = this.watchers.get(key);
    if (!set) {
      set = new Set();
      this.watchers.set(key, set);
    }
    set.add(cb);
    return () => { set!.delete(cb); };
  }

  state<T>(key: string) {
    return {
      key,
      watch: (cb: WatchCallback) => this.watch(key, cb),
      get:  () => null,
      isWatching: () => this.watchers.has(key),
      unwatch: () => this.watchers.delete(key),
      set: jest.fn().mockResolvedValue({}),
      patch: jest.fn().mockResolvedValue({}),
      setIn: jest.fn().mockResolvedValue({}),
      delete: jest.fn().mockResolvedValue({}),
      presence: jest.fn().mockResolvedValue({}),
    };
  }

  close() {}

  // Test helper: push a value to all watchers of a key
  _emit(key: string, value: unknown, meta: { version: number; initialized: boolean }) {
    const set = this.watchers.get(key);
    if (set) for (const cb of set) cb(value, meta);
  }
}

// Replace the real SodpClient import with the mock
jest.mock("@sodp/client", () => ({
  SodpClient: MockSodpClient,
}));

// ── Test helpers ───────────────────────────────────────────────────────────────

let mockClient: MockSodpClient | null = null;

// Intercept construction so we can call _emit() on the created client
const origMock = MockSodpClient;
jest.mock("@sodp/client", () => ({
  SodpClient: jest.fn().mockImplementation((...args: unknown[]) => {
    mockClient = new origMock(args[0] as string, args[1] as Record<string, unknown>);
    return mockClient;
  }),
}));

function wrapper({ children }: { children: ReactNode }) {
  return (
    <SODPProvider url="ws://test:7777">
      {children}
    </SODPProvider>
  );
}

// ── Tests ──────────────────────────────────────────────────────────────────────

beforeEach(() => {
  mockClient = null;
  jest.clearAllMocks();
});

describe("useSodpConnected", () => {
  it("starts as false, becomes true after onConnect fires", async () => {
    const { result } = renderHook(() => useSodpConnected(), { wrapper });

    // Initially disconnected
    expect(result.current).toBe(false);

    // Simulate connection
    await act(async () => {
      await new Promise(r => setTimeout(r, 10));
    });

    expect(result.current).toBe(true);
  });
});

describe("useSodpClient", () => {
  it("returns the client instance after mount", async () => {
    const { result } = renderHook(() => useSodpClient(), { wrapper });

    await act(async () => {
      await new Promise(r => setTimeout(r, 10));
    });

    expect(result.current).not.toBeNull();
  });
});

describe("useSodpState", () => {
  it("returns [null, null, null] initially", () => {
    const { result } = renderHook(() => useSodpState("test.key"), { wrapper });

    const [value, meta] = result.current;
    expect(value).toBeNull();
    expect(meta).toBeNull();
  });

  it("updates when a value is emitted", async () => {
    const { result } = renderHook(() => useSodpState<{ score: number }>("game.score"), { wrapper });

    // Wait for client to be created
    await act(async () => {
      await new Promise(r => setTimeout(r, 10));
    });

    // Emit a value
    act(() => {
      mockClient?._emit("game.score", { score: 42 }, { version: 1, initialized: true });
    });

    const [value, meta, ref] = result.current;
    expect(value).toEqual({ score: 42 });
    expect(meta?.version).toBe(1);
    expect(meta?.initialized).toBe(true);
    expect(ref).not.toBeNull();
  });

  it("resets value when key changes", async () => {
    let key = "key.a";
    const { result, rerender } = renderHook(() => useSodpState(key), { wrapper });

    await act(async () => {
      await new Promise(r => setTimeout(r, 10));
    });

    act(() => {
      mockClient?._emit("key.a", "value-a", { version: 1, initialized: true });
    });

    expect(result.current[0]).toBe("value-a");

    // Change key
    key = "key.b";
    rerender();

    // Value should reset to null
    expect(result.current[0]).toBeNull();
    expect(result.current[1]).toBeNull();
  });
});

describe("useSodpRef", () => {
  it("returns a StateRef with the correct key", async () => {
    const { result } = renderHook(() => useSodpRef<{ x: number }>("test.key"), { wrapper });

    await act(async () => {
      await new Promise(r => setTimeout(r, 10));
    });

    expect(result.current).not.toBeNull();
    expect(result.current!.key).toBe("test.key");
  });
});
