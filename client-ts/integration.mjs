/**
 * Integration test for the SODP TypeScript client.
 * Run: node integration.mjs
 */
import { SodpClient } from "./dist/index.js";
import WebSocket from "ws";
import { spawn } from "child_process";
import { createHmac } from "crypto";

function makeJwt(sub, secret, ttl = 3600) {
  const h = Buffer.from(JSON.stringify({ alg: "HS256", typ: "JWT" })).toString("base64url");
  const p = Buffer.from(JSON.stringify({ sub, exp: Math.floor(Date.now() / 1000) + ttl })).toString("base64url");
  const s = createHmac("sha256", secret).update(`${h}.${p}`).digest("base64url");
  return `${h}.${p}.${s}`;
}

const sleep = ms => new Promise(r => setTimeout(r, ms));

const PORT   = 7788;
const SECRET = "integ-test-secret";

let passed = 0, failed = 0;
function ok(label) { console.log(`  ✓ ${label}`); passed++; }
function fail(label, reason) { console.error(`  ✗ ${label}: ${reason}`); failed++; }
async function assert(label, fn) {
  try { await fn(); ok(label); }
  catch (e) { fail(label, e.message); }
}

console.log("Starting SODP server on port", PORT, "...");
const proc = spawn("../target/debug/sodp-server", [`0.0.0.0:${PORT}`], {
  env: { ...process.env, RUST_LOG: "error", SODP_JWT_SECRET: SECRET },
  stdio: "pipe",
});
proc.stderr.on("data", d => process.stderr.write(d));
await sleep(400);

function newClient(sub = "user") {
  return new SodpClient(`ws://localhost:${PORT}`, { WebSocket, token: makeJwt(sub, SECRET) });
}

// ── Test 1: StateRef watch + set ─────────────────────────────────────────────
console.log("\n── Test 1: StateRef watch + set ─────────────────────────────────");
await (async () => {
  const client = newClient("t1");
  await client.ready;

  const player = client.state("integ.player");
  const events = [];
  player.watch((v, meta) => events.push({ v, meta }));
  await sleep(100); // STATE_INIT

  await player.set({ name: "Alice", health: 100, position: { x: 0, y: 0 } });
  await sleep(100);

  await assert("STATE_INIT fires with initialized=false for new key", () => {
    if (events[0]?.meta?.initialized !== false) throw new Error(JSON.stringify(events[0]?.meta));
  });
  await assert("set() fires with full value", () => {
    if (events[1]?.v?.name !== "Alice") throw new Error(JSON.stringify(events[1]?.v));
  });
  client.close();
})();

await sleep(100);

// ── Test 2: Deep patch — only named fields updated ────────────────────────────
console.log("\n── Test 2: Deep patch — only named fields updated ───────────────");
await (async () => {
  const client = newClient("t2");
  await client.ready;

  const player = client.state("integ.patch");
  player.watch(() => {});
  await player.set({ name: "Bob", health: 100, position: { x: 0, y: 0 } });
  await sleep(100);

  // patch only health — name and position must survive
  await player.patch({ health: 80 });
  await sleep(100);

  await assert("patch preserves unmentioned fields", () => {
    const snap = player.get();
    if (snap?.name !== "Bob")        throw new Error(`name: ${snap?.name}`);
    if (snap?.health !== 80)         throw new Error(`health: ${snap?.health}`);
    if (snap?.position?.x !== 0)     throw new Error(`position.x: ${snap?.position?.x}`);
  });

  // deep patch — update only position.x, y must survive
  await player.patch({ position: { x: 99 } });
  await sleep(100);

  await assert("deep patch preserves sibling nested fields (position.y)", () => {
    const snap = player.get();
    if (snap?.position?.x !== 99) throw new Error(`x: ${snap?.position?.x}`);
    if (snap?.position?.y !== 0)  throw new Error(`y: ${snap?.position?.y}`);
  });

  client.close();
})();

await sleep(100);

// ── Test 3: setIn — atomic nested field update ───────────────────────────────
console.log("\n── Test 3: setIn — atomic nested field update ───────────────────");
await (async () => {
  const client = newClient("t3");
  await client.ready;

  const player = client.state("integ.setin");
  player.watch(() => {});
  await player.set({ name: "Carol", health: 100, position: { x: 0, y: 0 } });
  await sleep(100);

  await player.setIn("/position/x", 42);
  await sleep(100);

  await assert("setIn updates only the target field", () => {
    const snap = player.get();
    if (snap?.position?.x !== 42) throw new Error(`x: ${snap?.position?.x}`);
    if (snap?.position?.y !== 0)  throw new Error(`y: ${snap?.position?.y}`);
    if (snap?.name !== "Carol")   throw new Error(`name: ${snap?.name}`);
  });

  // setIn on a deeply nested path that doesn't exist yet
  await player.setIn("/stats/kills", 5);
  await sleep(100);

  await assert("setIn creates intermediate objects", () => {
    const snap = player.get();
    if (snap?.stats?.kills !== 5) throw new Error(JSON.stringify(snap?.stats));
  });

  client.close();
})();

await sleep(100);

// ── Test 4: delete() — true removal ─────────────────────────────────────────
console.log("\n── Test 4: delete() — true removal ──────────────────────────────");
await (async () => {
  const client = newClient("t4");
  await client.ready;

  const player = client.state("integ.delete");
  const events = [];
  player.watch((v, meta) => events.push({ v, meta }));

  await player.set({ name: "Dave" });
  await sleep(100);

  events.length = 0;
  await player.delete();
  await sleep(100);

  await assert("delete fires callback with null", () => {
    if (events[0]?.v !== null) throw new Error(`v: ${JSON.stringify(events[0]?.v)}`);
  });
  await assert("delete sets initialized=false", () => {
    if (events[0]?.meta?.initialized !== false) throw new Error(`initialized: ${events[0]?.meta?.initialized}`);
  });
  await assert("get() returns null after delete", () => {
    if (player.get() !== null) throw new Error(JSON.stringify(player.get()));
  });

  client.close();
})();

await sleep(100);

// ── Test 5: fanout across two clients ────────────────────────────────────────
console.log("\n── Test 5: fanout across two clients ─────────────────────────────");
await (async () => {
  const c1 = newClient("t5a"), c2 = newClient("t5b");
  await Promise.all([c1.ready, c2.ready]);

  const s1 = c1.state("integ.fanout2"), s2 = c2.state("integ.fanout2");
  const got2 = [];
  s2.watch(v => got2.push(v));
  await sleep(100);

  await s1.set({ score: 99 });
  await sleep(100);

  await assert("client2 receives client1's update", () => {
    const last = got2[got2.length - 1];
    if (last?.score !== 99) throw new Error(JSON.stringify(last));
  });

  c1.close(); c2.close();
})();

await sleep(100);

// ── Test 6: call() before connected is queued ────────────────────────────────
console.log("\n── Test 6: call() queued before connected ────────────────────────");
await (async () => {
  const client = newClient("t6");
  const scores = client.state("integ.queue2");
  scores.watch(() => {});

  // Fire immediately — not yet authenticated
  const p = scores.set({ queued: true });
  await p;
  await sleep(100);

  await assert("queued call executes after connect", () => {
    const snap = scores.get();
    if (snap?.queued !== true) throw new Error(JSON.stringify(snap));
  });

  client.close();
})();

await sleep(100);

// ── Test 7: ready + onConnect/onDisconnect ───────────────────────────────────
console.log("\n── Test 7: ready + onConnect/onDisconnect ────────────────────────");
await (async () => {
  let connects = 0, disconnects = 0;
  const client2 = new SodpClient(`ws://localhost:${PORT}`, {
    WebSocket,
    token: makeJwt("t7", SECRET),
    onConnect:    () => connects++,
    onDisconnect: () => disconnects++,
  });

  await client2.ready;
  await assert("ready resolves", () => {});
  await assert("onConnect fired", () => { if (connects !== 1) throw new Error(`connects=${connects}`); });

  client2.close();
  await sleep(100);
  await assert("onDisconnect fired", () => { if (disconnects !== 1) throw new Error(`disconnects=${disconnects}`); });
})();

await sleep(100);

// ── Test 8: isWatching + unsubscribe ─────────────────────────────────────────
console.log("\n── Test 8: isWatching + unsubscribe ─────────────────────────────");
await (async () => {
  const client = newClient("t8");
  await client.ready;

  const ref = client.state("integ.unsub2");
  await assert("isWatching false before watch()", () => {
    if (ref.isWatching()) throw new Error("should not be watching yet");
  });

  const unsub = ref.watch(() => {});
  await sleep(50);
  await assert("isWatching true after watch()", () => {
    if (!ref.isWatching()) throw new Error("should be watching");
  });

  unsub();
  // Note: isWatching stays true (subscription entry remains, just no callbacks)
  // This is by design — key cache is preserved even with 0 callbacks

  client.close();
})();

// ── Test 9: RESUME — delta replay after reconnect ─────────────────────────────
console.log("\n── Test 9: RESUME — delta replay after reconnect ────────────────");
await (async () => {
  // Promise that resolves the *second* time onConnect fires (= first reconnect).
  let connectCount = 0;
  let reconnectResolve;
  const reconnected = new Promise(r => { reconnectResolve = r; });

  const client = new SodpClient(`ws://localhost:${PORT}`, {
    WebSocket,
    token:          makeJwt("t9", SECRET),
    reconnect:      true,
    reconnectDelay: 100,
    onConnect: () => { if (++connectCount === 2) reconnectResolve(); },
  });
  await client.ready;

  const ref = client.state("integ.resume");
  const events = [];
  ref.watch((v, meta) => events.push({ v, meta }));
  await sleep(100); // STATE_INIT

  // Write version 1 while connected.
  await ref.set({ counter: 1 });
  await sleep(50);
  const vBefore = ref.get()?.counter;

  // Drop the connection — client will RESUME on reconnect.
  client["ws"]?.close();
  await sleep(50); // let disconnect event fire

  // Write version 2 while the client is offline.
  const c2 = newClient("t9b");
  await c2.ready;
  await c2.state("integ.resume").set({ counter: 2 });
  c2.close();

  // Wait for the client to reconnect and finish RESUME (not a fixed sleep).
  await reconnected;
  await sleep(150); // allow RESUME delta + STATE_INIT to be processed

  await assert("counter was 1 before disconnect", () => {
    if (vBefore !== 1) throw new Error(`vBefore=${vBefore}`);
  });
  await assert("after RESUME, counter reflects missed update (=2)", () => {
    const snap = ref.get();
    if (snap?.counter !== 2) throw new Error(`counter=${snap?.counter}`);
  });
  await assert("RESUME fires watch callback with missed delta", () => {
    const last = events[events.length - 1];
    if (last?.v?.counter !== 2) throw new Error(JSON.stringify(last));
  });

  client.close();
})();

// ── Test 10: UNWATCH — server stops sending DELTAs ───────────────────────────
console.log("\n── Test 10: UNWATCH — server stops sending DELTAs ──────────────");
await (async () => {
  const writer = newClient("t10w");
  const reader = newClient("t10r");
  await Promise.all([writer.ready, reader.ready]);

  const rRef = reader.state("integ.unwatch");
  const events = [];
  rRef.watch(v => events.push(v));
  await sleep(100); // STATE_INIT

  // Write 1 — reader is watching, should receive it.
  await writer.state("integ.unwatch").set({ n: 1 });
  await sleep(100);

  await assert("reader receives delta before unwatch", () => {
    const last = events[events.length - 1];
    if (last?.n !== 1) throw new Error(JSON.stringify(last));
  });

  // Unwatch — reader cancels its subscription.
  rRef.unwatch();
  await sleep(50); // give server time to process UNWATCH

  await assert("isWatching() is false after unwatch()", () => {
    if (rRef.isWatching()) throw new Error("should not be watching");
  });
  await assert("get() returns undefined after unwatch()", () => {
    if (rRef.get() !== undefined) throw new Error(`got: ${JSON.stringify(rRef.get())}`);
  });

  const countBefore = events.length;

  // Write 2 — reader is no longer subscribed, must NOT receive it.
  await writer.state("integ.unwatch").set({ n: 2 });
  await sleep(100);

  await assert("reader receives no more deltas after unwatch", () => {
    if (events.length !== countBefore) throw new Error(`got ${events.length - countBefore} unexpected event(s)`);
  });

  // Re-watching the same key after unwatch should work cleanly.
  const events2 = [];
  rRef.watch(v => events2.push(v));
  await sleep(100);

  await assert("re-watch after unwatch receives current value", () => {
    const last = events2[events2.length - 1];
    if (last?.n !== 2) throw new Error(JSON.stringify(last));
  });

  writer.close(); reader.close();
})();

await sleep(100);

// ── Summary ───────────────────────────────────────────────────────────────────
await sleep(100);
proc.kill();
console.log(`\n${"─".repeat(52)}`);
console.log(`Results: ${passed} passed, ${failed} failed`);
if (failed > 0) process.exit(1);
