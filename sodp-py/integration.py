"""
Integration test for the SODP Python client.
Run: python integration.py
"""

import asyncio
import base64
import hashlib
import hmac
import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "src"))
from sodp import SodpClient

# ── JWT helper ────────────────────────────────────────────────────────────────

def make_jwt(sub: str, secret: str, ttl: int = 3600) -> str:
    def b64(data: bytes) -> str:
        return base64.urlsafe_b64encode(data).rstrip(b"=").decode()

    header  = b64(json.dumps({"alg": "HS256", "typ": "JWT"}).encode())
    payload = b64(json.dumps({"sub": sub, "exp": int(time.time()) + ttl}).encode())
    msg     = f"{header}.{payload}".encode()
    sig     = b64(hmac.new(secret.encode(), msg, hashlib.sha256).digest())
    return f"{header}.{payload}.{sig}"

# ── Test harness ──────────────────────────────────────────────────────────────

passed = failed = 0

def ok(label: str) -> None:
    global passed
    print(f"  ✓ {label}")
    passed += 1

def fail(label: str, reason: str) -> None:
    global failed
    print(f"  ✗ {label}: {reason}", file=sys.stderr)
    failed += 1

async def assert_test(label: str, fn) -> None:
    try:
        result = fn()
        if asyncio.iscoroutine(result):
            await result
        ok(label)
    except Exception as e:
        fail(label, str(e))

PORT   = 7789
SECRET = "integ-test-secret-py"

def new_client(sub: str = "user") -> SodpClient:
    return SodpClient(
        f"ws://localhost:{PORT}",
        token=make_jwt(sub, SECRET),
    )

# ── Tests ─────────────────────────────────────────────────────────────────────

async def test1():
    print("\n── Test 1: StateRef watch + set ─────────────────────────────────")
    client = new_client("t1")
    await client.ready

    player = client.state("integ.player")
    events = []
    player.watch(lambda v, m: events.append((v, m)))
    await asyncio.sleep(0.1)   # STATE_INIT

    await player.set({"name": "Alice", "health": 100, "position": {"x": 0, "y": 0}})
    await asyncio.sleep(0.1)

    async def check_init():
        if events[0][1].initialized is not False:
            raise AssertionError(f"meta={events[0][1]}")
    await assert_test("STATE_INIT fires with initialized=False for new key", check_init)

    async def check_set():
        if events[1][0].get("name") != "Alice":
            raise AssertionError(f"v={events[1][0]}")
    await assert_test("set() fires with full value", check_set)

    client.close()


async def test2():
    print("\n── Test 2: Deep patch — only named fields updated ───────────────")
    client = new_client("t2")
    await client.ready

    player = client.state("integ.patch_py")
    player.watch(lambda v, m: None)
    await player.set({"name": "Bob", "health": 100, "position": {"x": 0, "y": 0}})
    await asyncio.sleep(0.1)

    await player.patch({"health": 80})
    await asyncio.sleep(0.1)

    async def check_patch():
        s = player.get()
        if s.get("name") != "Bob":   raise AssertionError(f"name={s.get('name')}")
        if s.get("health") != 80:    raise AssertionError(f"health={s.get('health')}")
        if s.get("position", {}).get("x") != 0:
            raise AssertionError(f"position.x={s.get('position',{}).get('x')}")
    await assert_test("patch preserves unmentioned fields", check_patch)

    await player.patch({"position": {"x": 99}})
    await asyncio.sleep(0.1)

    async def check_deep_patch():
        s = player.get()
        if s.get("position", {}).get("x") != 99:
            raise AssertionError(f"x={s.get('position',{}).get('x')}")
        if s.get("position", {}).get("y") != 0:
            raise AssertionError(f"y={s.get('position',{}).get('y')}")
    await assert_test("deep patch preserves sibling nested fields (position.y)", check_deep_patch)

    client.close()


async def test3():
    print("\n── Test 3: setIn — atomic nested field update ───────────────────")
    client = new_client("t3")
    await client.ready

    player = client.state("integ.setin_py")
    player.watch(lambda v, m: None)
    await player.set({"name": "Carol", "health": 100, "position": {"x": 0, "y": 0}})
    await asyncio.sleep(0.1)

    await player.set_in("/position/x", 42)
    await asyncio.sleep(0.1)

    async def check_setin():
        s = player.get()
        if s.get("position", {}).get("x") != 42:
            raise AssertionError(f"x={s.get('position',{}).get('x')}")
        if s.get("position", {}).get("y") != 0:
            raise AssertionError(f"y={s.get('position',{}).get('y')}")
        if s.get("name") != "Carol":
            raise AssertionError(f"name={s.get('name')}")
    await assert_test("setIn updates only the target field", check_setin)

    await player.set_in("/stats/kills", 5)
    await asyncio.sleep(0.1)

    async def check_setin_create():
        s = player.get()
        if s.get("stats", {}).get("kills") != 5:
            raise AssertionError(f"stats={s.get('stats')}")
    await assert_test("setIn creates intermediate objects", check_setin_create)

    client.close()


async def test4():
    print("\n── Test 4: delete() — true removal ──────────────────────────────")
    client = new_client("t4")
    await client.ready

    player = client.state("integ.delete_py")
    events = []
    player.watch(lambda v, m: events.append((v, m)))
    await player.set({"name": "Dave"})
    await asyncio.sleep(0.1)

    events.clear()
    await player.delete()
    await asyncio.sleep(0.1)

    async def check_null():
        if events[0][0] is not None:
            raise AssertionError(f"v={events[0][0]}")
    await assert_test("delete fires callback with None", check_null)

    async def check_uninit():
        if events[0][1].initialized is not False:
            raise AssertionError(f"initialized={events[0][1].initialized}")
    await assert_test("delete sets initialized=False", check_uninit)

    async def check_get():
        if player.get() is not None:
            raise AssertionError(f"get()={player.get()}")
    await assert_test("get() returns None after delete", check_get)

    client.close()


async def test5():
    print("\n── Test 5: fanout across two clients ─────────────────────────────")
    c1 = new_client("t5a")
    c2 = new_client("t5b")
    await asyncio.gather(c1.ready, c2.ready)

    s1 = c1.state("integ.fanout_py")
    s2 = c2.state("integ.fanout_py")
    got2 = []
    s2.watch(lambda v, m: got2.append(v))
    await asyncio.sleep(0.1)

    await s1.set({"score": 99})
    await asyncio.sleep(0.1)

    async def check_fanout():
        last = got2[-1] if got2 else None
        if not last or last.get("score") != 99:
            raise AssertionError(f"last={last}")
    await assert_test("client2 receives client1's update", check_fanout)

    c1.close()
    c2.close()


async def test6():
    print("\n── Test 6: call() queued before connected ────────────────────────")
    client = new_client("t6")
    scores = client.state("integ.queue_py")
    scores.watch(lambda v, m: None)

    # Fire immediately — not yet authenticated.
    task = asyncio.create_task(scores.set({"queued": True}))
    await task
    await asyncio.sleep(0.1)

    async def check_queued():
        s = scores.get()
        if not s or s.get("queued") is not True:
            raise AssertionError(f"snap={s}")
    await assert_test("queued call executes after connect", check_queued)

    client.close()


async def test7():
    print("\n── Test 7: ready + on_connect/on_disconnect ──────────────────────")
    connects = disconnects = 0

    def on_conn():    nonlocal connects;    connects    += 1
    def on_disc():    nonlocal disconnects; disconnects += 1

    client = SodpClient(
        f"ws://localhost:{PORT}",
        token=make_jwt("t7", SECRET),
        on_connect=on_conn,
        on_disconnect=on_disc,
    )
    await client.ready

    await assert_test("ready resolves", lambda: None)

    async def check_connect():
        if connects != 1:
            raise AssertionError(f"connects={connects}")
    await assert_test("on_connect fired", check_connect)

    client.close()
    await asyncio.sleep(0.2)

    async def check_disconnect():
        if disconnects != 1:
            raise AssertionError(f"disconnects={disconnects}")
    await assert_test("on_disconnect fired", check_disconnect)


async def test8():
    print("\n── Test 8: is_watching + unsubscribe ────────────────────────────")
    client = new_client("t8")
    await client.ready

    ref = client.state("integ.unsub_py")

    async def check_before():
        if ref.is_watching():
            raise AssertionError("should not be watching yet")
    await assert_test("is_watching False before watch()", check_before)

    unsub = ref.watch(lambda v, m: None)
    await asyncio.sleep(0.05)

    async def check_after():
        if not ref.is_watching():
            raise AssertionError("should be watching")
    await assert_test("is_watching True after watch()", check_after)

    unsub()  # removes the callback; server subscription stays alive

    client.close()


async def test9():
    print("\n── Test 9: RESUME — delta replay after reconnect ────────────────")
    connect_count = 0
    reconnected   = asyncio.Event()

    def on_conn():
        nonlocal connect_count
        connect_count += 1
        if connect_count == 2:
            reconnected.set()

    client = SodpClient(
        f"ws://localhost:{PORT}",
        token=make_jwt("t9", SECRET),
        reconnect=True,
        reconnect_delay=0.1,
        on_connect=on_conn,
    )
    await client.ready

    ref    = client.state("integ.resume_py")
    events = []
    ref.watch(lambda v, m: events.append((v, m)))
    await asyncio.sleep(0.1)   # STATE_INIT

    await ref.set({"counter": 1})
    await asyncio.sleep(0.05)
    v_before = (ref.get() or {}).get("counter")

    # Drop the connection — client will RESUME on reconnect.
    if client._ws:
        await client._ws.close()
    await asyncio.sleep(0.05)

    # Write version 2 while the client is offline.
    c2 = new_client("t9b")
    await c2.ready
    await c2.state("integ.resume_py").set({"counter": 2})
    c2.close()

    # Wait for the client to reconnect and finish RESUME.
    await asyncio.wait_for(reconnected.wait(), timeout=5.0)
    await asyncio.sleep(0.2)   # allow RESUME delta + STATE_INIT to be processed

    async def check_before():
        if v_before != 1:
            raise AssertionError(f"v_before={v_before}")
    await assert_test("counter was 1 before disconnect", check_before)

    async def check_resume():
        s = ref.get()
        if not s or s.get("counter") != 2:
            raise AssertionError(f"counter={s}")
    await assert_test("after RESUME, counter reflects missed update (=2)", check_resume)

    async def check_callback():
        last = events[-1] if events else None
        if not last or (last[0] or {}).get("counter") != 2:
            raise AssertionError(f"last={last}")
    await assert_test("RESUME fires watch callback with missed delta", check_callback)

    client.close()


async def test10():
    print("\n── Test 10: UNWATCH — server stops sending DELTAs ──────────────")
    writer = new_client("t10w")
    reader = new_client("t10r")
    await asyncio.gather(writer.ready, reader.ready)

    r_ref  = reader.state("integ.unwatch_py")
    events = []
    r_ref.watch(lambda v, m: events.append(v))
    await asyncio.sleep(0.1)

    await writer.state("integ.unwatch_py").set({"n": 1})
    await asyncio.sleep(0.1)

    async def check_before():
        last = events[-1] if events else None
        if not last or last.get("n") != 1:
            raise AssertionError(f"last={last}")
    await assert_test("reader receives delta before unwatch", check_before)

    r_ref.unwatch()
    await asyncio.sleep(0.05)

    async def check_not_watching():
        if r_ref.is_watching():
            raise AssertionError("should not be watching")
    await assert_test("is_watching() is False after unwatch()", check_not_watching)

    async def check_get_none():
        if r_ref.get() is not None:
            raise AssertionError(f"got: {r_ref.get()}")
    await assert_test("get() returns None after unwatch()", check_get_none)

    count_before = len(events)
    await writer.state("integ.unwatch_py").set({"n": 2})
    await asyncio.sleep(0.1)

    async def check_no_more():
        if len(events) != count_before:
            raise AssertionError(f"got {len(events) - count_before} unexpected event(s)")
    await assert_test("reader receives no more deltas after unwatch", check_no_more)

    # Re-watch should work cleanly.
    events2 = []
    r_ref.watch(lambda v, m: events2.append(v))
    await asyncio.sleep(0.1)

    async def check_rewatch():
        last = events2[-1] if events2 else None
        if not last or last.get("n") != 2:
            raise AssertionError(f"last={last}")
    await assert_test("re-watch after unwatch receives current value", check_rewatch)

    writer.close()
    reader.close()


# ── Main ──────────────────────────────────────────────────────────────────────

async def main():
    server_bin = os.path.join(
        os.path.dirname(__file__), "..", "target", "debug", "sodp-server"
    )
    print(f"Starting SODP server on port {PORT} …")
    proc = subprocess.Popen(
        [server_bin, f"0.0.0.0:{PORT}"],
        env={**os.environ, "RUST_LOG": "error", "SODP_JWT_SECRET": SECRET},
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    try:
        await asyncio.sleep(0.4)

        await test1();  await asyncio.sleep(0.1)
        await test2();  await asyncio.sleep(0.1)
        await test3();  await asyncio.sleep(0.1)
        await test4();  await asyncio.sleep(0.1)
        await test5();  await asyncio.sleep(0.1)
        await test6();  await asyncio.sleep(0.1)
        await test7();  await asyncio.sleep(0.1)
        await test8();  await asyncio.sleep(0.1)
        await test9();  await asyncio.sleep(0.1)
        await test10(); await asyncio.sleep(0.1)

    finally:
        proc.terminate()
        proc.wait()

    print(f"\n{'─' * 52}")
    print(f"Results: {passed} passed, {failed} failed")
    if failed:
        sys.exit(1)


if __name__ == "__main__":
    asyncio.run(main())
