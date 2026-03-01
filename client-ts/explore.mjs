/**
 * Exploratory script — tests every nested op pattern against the live server.
 * Run: node explore.mjs
 */
import { SodpClient } from "./dist/index.js";
import WebSocket from "ws";
import { execSync, spawn } from "child_process";
import { createHmac } from "crypto";

function makeJwt(sub, secret, ttl = 3600) {
  const h = Buffer.from(JSON.stringify({ alg: "HS256", typ: "JWT" })).toString("base64url");
  const p = Buffer.from(JSON.stringify({ sub, exp: Math.floor(Date.now() / 1000) + ttl })).toString("base64url");
  const s = createHmac("sha256", secret).update(`${h}.${p}`).digest("base64url");
  return `${h}.${p}.${s}`;
}

const sleep = ms => new Promise(r => setTimeout(r, ms));

const PORT   = 7790;
const SECRET = "explore-secret";

const proc = spawn("../target/debug/sodp-server", [`0.0.0.0:${PORT}`], {
  env: { ...process.env, RUST_LOG: "error", SODP_JWT_SECRET: SECRET },
  stdio: "pipe",
});
await sleep(300);

const token  = makeJwt("dev", SECRET);
const client = new SodpClient(`ws://localhost:${PORT}`, { WebSocket, token });
await client.ready;

const deltas = [];
client.watch("player", (v, meta) => deltas.push({ v: JSON.parse(JSON.stringify(v)), meta }));
await sleep(100); // STATE_INIT

// ─────────────────────────────────────────────────────────────────────────────
console.log("\n══ 1. Set a full nested object ══════════════════════════════════");
deltas.length = 0;
await client.set("player", {
  name:      "Alice",
  health:    100,
  position:  { x: 0, y: 0 },
  inventory: ["sword", "shield"],
});
await sleep(80);
console.log("value  →", JSON.stringify(deltas[0]?.v));
console.log("expect →", JSON.stringify({ name:"Alice", health:100, position:{x:0,y:0}, inventory:["sword","shield"] }));

// ─────────────────────────────────────────────────────────────────────────────
console.log("\n══ 2. Shallow patch — update top-level fields only ═════════════");
deltas.length = 0;
await client.patch("player", { health: 80 });
await sleep(80);
console.log("delta ops sent  →", deltas[0]?.v);
console.log("expect position → unchanged:", deltas[0]?.v?.position);
console.log("expect health   → 80:       ", deltas[0]?.v?.health);

// ─────────────────────────────────────────────────────────────────────────────
console.log("\n══ 3. Patch — replace a nested object entirely ═════════════════");
deltas.length = 0;
// patch is SHALLOW — the whole position object is replaced, not just x
await client.patch("player", { position: { x: 5, y: 0 } });
await sleep(80);
console.log("value  →", JSON.stringify(deltas[0]?.v?.position));
console.log("expect → {x:5, y:0}  (y reverts to 0 because whole sub-object replaced)");

// ─────────────────────────────────────────────────────────────────────────────
console.log("\n══ 4. Update a single nested field — read-modify-write ══════════");
deltas.length = 0;
// No direct API for this — must read snapshot and set
const snap = client.getSnapshot("player");
await client.patch("player", {
  position: { ...snap.position, x: 99 },  // spread to keep y
});
await sleep(80);
console.log("value  →", JSON.stringify(deltas[0]?.v?.position));
console.log("expect → {x:99, y:0}  (only x changed)");

// ─────────────────────────────────────────────────────────────────────────────
console.log("\n══ 5. Deeply nested object update ═══════════════════════════════");
// Watch first, then set — so getSnapshot works
client.watch("world", () => {});
await client.set("world", {
  zones: {
    forest: { enemies: 3, boss: { name: "Goblin King", hp: 500 } },
    desert: { enemies: 1, boss: null },
  },
});
await sleep(80);

// Update only the boss hp deep inside — read-modify-write with spread
const world = client.getSnapshot("world");
await client.set("world", {
  ...world,
  zones: {
    ...world.zones,
    forest: {
      ...world.zones.forest,
      boss: { ...world.zones.forest.boss, hp: 450 },
    },
  },
});
await sleep(80);
const boss = client.getSnapshot("world")?.zones?.forest?.boss;
console.log("boss   →", JSON.stringify(boss));
console.log("expect → {name:'Goblin King', hp:450}");

// ─────────────────────────────────────────────────────────────────────────────
console.log("\n══ 6. Delete — set to null (no true delete in protocol) ════════");
deltas.length = 0;
await client.set("player", null);
await sleep(80);
console.log("value       →", deltas[0]?.v);
console.log("initialized →", deltas[0]?.meta?.initialized, " (true: key exists as null)");
console.log("expect value → null, initialized → true");

// ─────────────────────────────────────────────────────────────────────────────
console.log("\n══ 7. Arrays — full replacement only ════════════════════════════");
await client.set("player", { name: "Alice", health: 100, inventory: ["sword", "shield"] });
await sleep(80);
deltas.length = 0; // reset after set so only the patch delta is captured

const before = client.getSnapshot("player");
// Add an item — must spread the array; no item-level ops exist
await client.patch("player", {
  inventory: [...before.inventory, "potion"],
});
await sleep(80);
console.log("inventory →", JSON.stringify(deltas[0]?.v?.inventory));
console.log("expect    → [\"sword\",\"shield\",\"potion\"]");

// ─────────────────────────────────────────────────────────────────────────────
console.log("\n══ 8. Watch multiple keys independently ════════════════════════");
const scores = [], chat = [];
client.watch("game.scores", v => scores.push(v));
client.watch("game.chat",   v => chat.push(v));
await sleep(100);

await client.set("game.scores", { alice: 100, bob: 80 });
await client.set("game.chat",   { last: "Hello!" });
await sleep(100);

console.log("scores →", JSON.stringify(scores[scores.length - 1]));
console.log("chat   →", JSON.stringify(chat[chat.length - 1]));
console.log("expect → independent updates, correct values in each callback");

// ─────────────────────────────────────────────────────────────────────────────
console.log("\n══ 9. call() escape hatch — custom server methods ══════════════");
// call() is still available for custom/future methods
const result = await client.call("state.set", { state: "game.scores", value: { alice: 200, bob: 80 } });
await sleep(80);
console.log("call result →", JSON.stringify(result));

// ─────────────────────────────────────────────────────────────────────────────
console.log("\n══ Gaps found ═══════════════════════════════════════════════════");
console.log("❌  No client.delete(key)        — workaround: set(key, null)");
console.log("❌  No setIn(key, path, value)   — workaround: read → spread → set");
console.log("❌  No patchIn(key, path, patch) — workaround: read → spread → patch");
console.log("❌  No array item ops            — workaround: read → spread array → patch");
console.log("⚠️  patch() is shallow-only       — nested objects replaced entirely");

client.close();
proc.kill();
