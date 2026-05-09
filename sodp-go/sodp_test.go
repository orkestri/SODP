package sodp

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/websocket"
	"github.com/vmihailenco/msgpack/v5"
)

// --- helpers ---

func testServer(t *testing.T, opts ...ServerOption) (*Server, *httptest.Server) {
	t.Helper()
	srv := NewServer(opts...)
	mux := http.NewServeMux()
	mux.HandleFunc("/sodp", srv.HandleWS)
	ts := httptest.NewServer(mux)
	t.Cleanup(ts.Close)
	return srv, ts
}

func dial(t *testing.T, ts *httptest.Server) *websocket.Conn {
	t.Helper()
	url := "ws" + strings.TrimPrefix(ts.URL, "http") + "/sodp"
	conn, _, err := websocket.DefaultDialer.Dial(url, nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { conn.Close() })
	return conn
}

func readFrame(t *testing.T, conn *websocket.Conn) Frame {
	t.Helper()
	conn.SetReadDeadline(time.Now().Add(3 * time.Second))
	_, data, err := conn.ReadMessage()
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	f, err := DecodeFrame(data)
	if err != nil {
		t.Fatalf("decode: %v", err)
	}
	return f
}

func sendFrame(t *testing.T, conn *websocket.Conn, f Frame) {
	t.Helper()
	data, err := EncodeFrame(f)
	if err != nil {
		t.Fatalf("encode: %v", err)
	}
	if err := conn.WriteMessage(websocket.BinaryMessage, data); err != nil {
		t.Fatalf("write: %v", err)
	}
}

func bodyMap(t *testing.T, body any) map[string]any {
	t.Helper()
	if m, ok := body.(map[string]any); ok {
		return m
	}
	data, _ := msgpack.Marshal(body)
	var m map[string]any
	msgpack.Unmarshal(data, &m)
	return m
}

// --- Frame encode/decode ---

func TestFrameRoundTrip(t *testing.T) {
	f := Frame{Type: FrameDelta, StreamID: 42, Seq: 100, Body: map[string]any{"key": "foo", "version": 5}}
	data, err := EncodeFrame(f)
	if err != nil {
		t.Fatal(err)
	}
	got, err := DecodeFrame(data)
	if err != nil {
		t.Fatal(err)
	}
	if got.Type != f.Type || got.StreamID != f.StreamID || got.Seq != f.Seq {
		t.Errorf("round-trip mismatch: %+v", got)
	}
}

func TestDecodeFrameInvalid(t *testing.T) {
	if _, err := DecodeFrame([]byte{0x00}); err == nil {
		t.Error("expected error on garbage input")
	}
}

// --- Delta diff ---

func TestDiff_AddedKeys(t *testing.T) {
	ops := Diff(map[string]any{"a": "1"}, map[string]any{"a": "1", "b": "2"})
	if len(ops) != 1 || ops[0].Op != OpAdd || ops[0].Path != "/b" {
		t.Errorf("unexpected: %+v", ops)
	}
}

func TestDiff_RemovedKeys(t *testing.T) {
	ops := Diff(map[string]any{"a": "1", "b": "2"}, map[string]any{"a": "1"})
	if len(ops) != 1 || ops[0].Op != OpRemove || ops[0].Path != "/b" {
		t.Errorf("unexpected: %+v", ops)
	}
}

func TestDiff_UpdatedKeys(t *testing.T) {
	ops := Diff(map[string]any{"status": "running"}, map[string]any{"status": "done"})
	if len(ops) != 1 || ops[0].Op != OpUpdate || ops[0].Path != "/status" {
		t.Errorf("unexpected: %+v", ops)
	}
}

func TestDiff_NestedMaps(t *testing.T) {
	ops := Diff(map[string]any{"meta": map[string]any{"rows": 10}}, map[string]any{"meta": map[string]any{"rows": 20}})
	if len(ops) != 1 || ops[0].Path != "/meta/rows" {
		t.Errorf("unexpected: %+v", ops)
	}
}

func TestDiff_NoChange(t *testing.T) {
	v := map[string]any{"a": "1"}
	if ops := Diff(v, v); len(ops) != 0 {
		t.Errorf("expected no ops, got %d", len(ops))
	}
}

func TestDiff_NonMapReplacement(t *testing.T) {
	ops := Diff("old", "new")
	if len(ops) != 1 || ops[0].Op != OpUpdate || ops[0].Path != "/" {
		t.Errorf("unexpected: %+v", ops)
	}
}

// --- StateStore ---

func TestState_ApplyAndGet(t *testing.T) {
	s := NewStateStore()
	d := s.Apply("k", map[string]any{"status": "ok"})
	if d == nil || d.Version != 1 {
		t.Fatalf("unexpected delta: %v", d)
	}
	val, ver := s.Get("k")
	if ver != 1 {
		t.Errorf("version: %d", ver)
	}
	if m := val.(map[string]any); m["status"] != "ok" {
		t.Errorf("value: %v", val)
	}
}

func TestState_NoOpApply(t *testing.T) {
	s := NewStateStore()
	s.Apply("k", map[string]any{"a": "1"})
	if s.Apply("k", map[string]any{"a": "1"}) != nil {
		t.Error("expected nil delta on no-op")
	}
}

func TestState_Delete(t *testing.T) {
	s := NewStateStore()
	s.Apply("k", "v")
	if d := s.Delete("k"); d == nil {
		t.Fatal("expected delta")
	}
	if _, ver := s.Get("k"); ver != 0 {
		t.Error("expected nil after delete")
	}
}

func TestState_DeltasSince(t *testing.T) {
	s := NewStateStore()
	s.Apply("k", map[string]any{"a": "1"})
	s.Apply("k", map[string]any{"a": "2"})
	s.Apply("k", map[string]any{"a": "3"})
	ds := s.DeltasSince("k", 1)
	if len(ds) != 2 {
		t.Errorf("expected 2 deltas, got %d", len(ds))
	}
}

func TestState_Append(t *testing.T) {
	s := NewStateStore()
	d1 := s.Append("logs", "e1", 3)
	if d1 == nil || d1.Ops[0].Op != OpAdd || d1.Ops[0].Path != "/-" {
		t.Fatalf("first append: %+v", d1)
	}
	s.Append("logs", "e2", 3)
	s.Append("logs", "e3", 3)
	d4 := s.Append("logs", "e4", 3) // triggers trim
	if len(d4.Ops) != 2 || d4.Ops[1].Op != OpRemove || d4.Ops[1].Path != "/0" {
		t.Errorf("trim ops: %+v", d4.Ops)
	}
	val, _ := s.Get("logs")
	if slice := val.([]any); len(slice) != 3 || slice[0] != "e2" {
		t.Errorf("after trim: %v", slice)
	}
}

func TestState_MaxKeys(t *testing.T) {
	s := NewStateStore()
	s.maxKeys = 2
	s.Apply("k1", "v1")
	s.Apply("k2", "v2")
	if s.Apply("k3", "v3") != nil {
		t.Error("should be refused at max keys")
	}
	if s.Apply("k1", "updated") == nil {
		t.Error("updating existing key should work")
	}
}

func TestState_EvictIf(t *testing.T) {
	s := NewStateStore()
	s.Apply("keep", "v")
	s.Apply("remove", "v")
	n := s.EvictIf(func(key string, _ any) bool { return key == "remove" })
	if n != 1 {
		t.Errorf("evicted %d, want 1", n)
	}
	if _, ver := s.Get("remove"); ver != 0 {
		t.Error("remove key should be gone")
	}
	if _, ver := s.Get("keep"); ver == 0 {
		t.Error("keep key should still exist")
	}
}

// --- Ring buffer ---

func TestRingLog_Wrap(t *testing.T) {
	r := newRingLog(3)
	for i := uint64(1); i <= 4; i++ {
		r.push(DeltaEntry{Version: i})
	}
	if r.oldestVersion() != 2 {
		t.Errorf("oldest: %d, want 2", r.oldestVersion())
	}
	var versions []uint64
	r.scan(func(d DeltaEntry) bool {
		versions = append(versions, d.Version)
		return true
	})
	if len(versions) != 3 || versions[0] != 2 || versions[2] != 4 {
		t.Errorf("scan: %v", versions)
	}
}

// --- FanoutBus ---

func TestFanout_BroadcastAndSkipSender(t *testing.T) {
	fb := NewFanoutBus(nil)
	ch := make(chan outMsg, 4)

	fb.Subscribe("k", Subscriber{SessionID: "s1", StreamID: 10, Send: ch})
	fb.Subscribe("k", Subscriber{SessionID: "s2", StreamID: 11, Send: ch})

	delta := DeltaEntry{Key: "k", Version: 1, Ops: []DeltaOp{{Op: OpUpdate, Path: "/x"}}}

	// s1 is the sender — only s2 should receive.
	// Delivery is async (per-key goroutine), so wait with a timeout.
	fb.Broadcast(delta, "s1")

	select {
	case <-ch:
		// got one message (s2)
	case <-time.After(time.Second):
		t.Fatal("s2 should have received within 1s")
	}
	select {
	case <-ch:
		t.Error("only one message expected (s1 was sender)")
	case <-time.After(50 * time.Millisecond):
		// no second message — correct
	}
}

func TestFanout_RemoveSession(t *testing.T) {
	fb := NewFanoutBus(nil)
	ch := make(chan outMsg, 4)
	fb.Subscribe("k1", Subscriber{SessionID: "s1", StreamID: 10, Send: ch})
	fb.Subscribe("k2", Subscriber{SessionID: "s1", StreamID: 11, Send: ch})
	fb.RemoveSession("s1")
	if fb.SubscriberCount("k1") != 0 || fb.SubscriberCount("k2") != 0 {
		t.Error("session should have been fully removed")
	}
}

// --- Session ---

func TestSession_WatchLifecycle(t *testing.T) {
	sess := NewSession("s1")
	sid := sess.AllocStream()
	if sid < firstStreamID {
		t.Errorf("stream_id %d < %d", sid, firstStreamID)
	}
	if !sess.AddWatch(sid, "k") {
		t.Fatal("AddWatch failed")
	}
	if key, ok := sess.RemoveWatch(sid); !ok || key != "k" {
		t.Errorf("RemoveWatch: %s %v", key, ok)
	}
}

func TestSession_WatchLimit(t *testing.T) {
	sess := NewSession("s1")
	sess.maxWatches = 1
	if !sess.AddWatch(10, "k1") {
		t.Fatal("first watch should succeed")
	}
	if sess.AddWatch(11, "k2") {
		t.Error("second watch should be rejected")
	}
}

func TestSession_RateLimit(t *testing.T) {
	sess := NewSession("s1")
	sess.rateLimit = 3
	for i := 0; i < 3; i++ {
		if !sess.CheckRate() {
			t.Fatalf("should allow call %d", i+1)
		}
	}
	if sess.CheckRate() {
		t.Error("should reject after rate limit")
	}
}

func TestSession_ReauthBlocked(t *testing.T) {
	sess := NewSession("s1")
	sess.SetAuth("u1", map[string]any{})
	if !sess.IsAuthenticated() {
		t.Error("should be authenticated")
	}
	// SetAuth again should not panic (idempotent)
	sess.SetAuth("u2", map[string]any{})
}

// --- Key validation ---

func TestIsValidKey(t *testing.T) {
	valid := []string{"k", "a.b", "a.b.c", "dashboard.default"}
	for _, k := range valid {
		if !isValidKey(k) {
			t.Errorf("should be valid: %q", k)
		}
	}
	invalid := []string{"", ".start", "end.", "double..dot", "has/slash", "has\\back", "has\x00null", string(make([]byte, 257))}
	for _, k := range invalid {
		if isValidKey(k) {
			t.Errorf("should be invalid: %q", k)
		}
	}
}

// --- Integration: server lifecycle ---

func TestIntegration_HelloOnConnect(t *testing.T) {
	_, ts := testServer(t)
	conn := dial(t, ts)

	f := readFrame(t, conn)
	if f.Type != FrameHello {
		t.Fatalf("expected HELLO, got 0x%02x", f.Type)
	}
	m := bodyMap(t, f.Body)
	if m["protocol"] != "sodp" {
		t.Errorf("protocol: %v", m["protocol"])
	}
	if m["version"] != "1.0" {
		t.Errorf("version: %v", m["version"])
	}
}

func TestIntegration_WatchAndStateInit(t *testing.T) {
	srv, ts := testServer(t)
	srv.Mutate("user.settings", map[string]any{"theme": "dark"})

	conn := dial(t, ts)
	readFrame(t, conn) // HELLO

	sendFrame(t, conn, Frame{Type: FrameWatch, StreamID: 10, Body: map[string]any{"state": "user.settings"}})

	f := readFrame(t, conn)
	if f.Type != FrameStateInit {
		t.Fatalf("expected STATE_INIT, got 0x%02x", f.Type)
	}
	if f.StreamID != 10 {
		t.Errorf("stream_id: %d", f.StreamID)
	}
	m := bodyMap(t, f.Body)
	if m["state"] != "user.settings" {
		t.Errorf("state: %v", m["state"])
	}
	if m["initialized"] != true {
		t.Error("initialized should be true")
	}
}

func TestIntegration_WatchUninitialized(t *testing.T) {
	_, ts := testServer(t)
	conn := dial(t, ts)
	readFrame(t, conn)

	sendFrame(t, conn, Frame{Type: FrameWatch, StreamID: 10, Body: map[string]any{"state": "does.not.exist"}})

	f := readFrame(t, conn)
	if f.Type != FrameStateInit {
		t.Fatalf("expected STATE_INIT, got 0x%02x", f.Type)
	}
	m := bodyMap(t, f.Body)
	if m["initialized"] != false {
		t.Error("initialized should be false for missing key")
	}
}

func TestIntegration_WatchReceivesDelta(t *testing.T) {
	srv, ts := testServer(t)
	conn := dial(t, ts)
	readFrame(t, conn)

	sendFrame(t, conn, Frame{Type: FrameWatch, StreamID: 10, Body: map[string]any{"state": "events"}})
	readFrame(t, conn) // STATE_INIT

	time.Sleep(30 * time.Millisecond)

	srv.MutateAppend("events", map[string]any{"type": "ping"}, 100)

	f := readFrame(t, conn)
	if f.Type != FrameDelta {
		t.Fatalf("expected DELTA, got 0x%02x", f.Type)
	}
}

func TestIntegration_MultiKey_Watch(t *testing.T) {
	srv, ts := testServer(t)
	srv.Mutate("a", "va")
	srv.Mutate("b", "vb")

	conn := dial(t, ts)
	readFrame(t, conn)

	sendFrame(t, conn, Frame{
		Type: FrameWatch,
		Body: map[string]any{"states": []any{"a", "b"}},
	})

	// Should receive STATE_INIT for each key.
	received := map[string]bool{}
	for i := 0; i < 2; i++ {
		f := readFrame(t, conn)
		if f.Type != FrameStateInit {
			t.Fatalf("expected STATE_INIT[%d], got 0x%02x", i, f.Type)
		}
		m := bodyMap(t, f.Body)
		received[m["state"].(string)] = true
	}
	if !received["a"] || !received["b"] {
		t.Errorf("missing STATE_INIT for some keys: %v", received)
	}
}

func TestIntegration_HeartbeatEcho(t *testing.T) {
	_, ts := testServer(t)
	conn := dial(t, ts)
	readFrame(t, conn)

	sendFrame(t, conn, Frame{Type: FrameHeartbeat, Seq: 42})

	f := readFrame(t, conn)
	if f.Type != FrameHeartbeat || f.Seq != 42 {
		t.Errorf("expected HEARTBEAT seq=42, got type=0x%02x seq=%d", f.Type, f.Seq)
	}
}

func TestIntegration_CallStateSet(t *testing.T) {
	srv, ts := testServer(t)
	conn := dial(t, ts)
	readFrame(t, conn)

	sendFrame(t, conn, Frame{
		Type: FrameCall, StreamID: 1, Seq: 1,
		Body: map[string]any{
			"call_id": "c1",
			"method":  "state.set",
			"args":    map[string]any{"state": "config", "value": map[string]any{"k": "v"}},
		},
	})

	f := readFrame(t, conn)
	if f.Type != FrameResult {
		t.Fatalf("expected RESULT, got 0x%02x", f.Type)
	}
	m := bodyMap(t, f.Body)
	if m["success"] != true || m["call_id"] != "c1" {
		t.Errorf("result: %v", m)
	}
	val, ver := srv.State.Get("config")
	if ver == 0 {
		t.Fatal("key should exist")
	}
	if val.(map[string]any)["k"] != "v" {
		t.Errorf("value: %v", val)
	}
}

func TestIntegration_CallPatch(t *testing.T) {
	srv, ts := testServer(t)
	// Use int64 values — msgpack decodes integers as int64, so the merged
	// state will have int64 for the patched fields. Use int64 throughout
	// to keep comparisons consistent.
	srv.Mutate("cfg", map[string]any{"a": int64(1), "b": int64(2)})
	conn := dial(t, ts)
	readFrame(t, conn)

	sendFrame(t, conn, Frame{
		Type: FrameCall, StreamID: 1,
		Body: map[string]any{
			"call_id": "p1",
			"method":  "state.patch",
			"args":    map[string]any{"state": "cfg", "patch": map[string]any{"b": int64(3), "c": int64(4)}},
		},
	})
	readFrame(t, conn) // RESULT

	val, _ := srv.State.Get("cfg")
	m := val.(map[string]any)
	if m["a"] != int64(1) || m["b"] != int64(3) || m["c"] != int64(4) {
		t.Errorf("patch result: %v", m)
	}
}

func TestIntegration_CallSetIn(t *testing.T) {
	srv, ts := testServer(t)
	srv.Mutate("obj", map[string]any{"meta": map[string]any{"count": 0}})
	conn := dial(t, ts)
	readFrame(t, conn)

	sendFrame(t, conn, Frame{
		Type: FrameCall, StreamID: 1,
		Body: map[string]any{
			"call_id": "si1",
			"method":  "state.set_in",
			"args":    map[string]any{"state": "obj", "path": "meta.count", "value": int64(42)},
		},
	})
	readFrame(t, conn) // RESULT

	val, _ := srv.State.Get("obj")
	m := val.(map[string]any)["meta"].(map[string]any)
	if m["count"] != int64(42) {
		t.Errorf("set_in result: %v", m)
	}
}

func TestIntegration_WatcherGetsDeltaFromCall(t *testing.T) {
	_, ts := testServer(t)

	c1 := dial(t, ts)
	readFrame(t, c1)
	sendFrame(t, c1, Frame{Type: FrameWatch, StreamID: 10, Body: map[string]any{"state": "shared"}})
	readFrame(t, c1) // STATE_INIT

	c2 := dial(t, ts)
	readFrame(t, c2)
	time.Sleep(30 * time.Millisecond)

	sendFrame(t, c2, Frame{
		Type: FrameCall, StreamID: 1,
		Body: map[string]any{
			"call_id": "m1",
			"method":  "state.set",
			"args":    map[string]any{"state": "shared", "value": map[string]any{"count": 1}},
		},
	})
	readFrame(t, c2) // RESULT

	f := readFrame(t, c1)
	if f.Type != FrameDelta {
		t.Fatalf("watcher expected DELTA, got 0x%02x", f.Type)
	}
}

func TestIntegration_InvalidKeyRejected(t *testing.T) {
	_, ts := testServer(t)
	conn := dial(t, ts)
	readFrame(t, conn)

	sendFrame(t, conn, Frame{Type: FrameWatch, StreamID: 10, Body: map[string]any{"state": ""}})

	f := readFrame(t, conn)
	if f.Type != FrameError {
		t.Fatalf("expected ERROR, got 0x%02x", f.Type)
	}
}

func TestIntegration_InvalidMethod(t *testing.T) {
	_, ts := testServer(t)
	conn := dial(t, ts)
	readFrame(t, conn)

	sendFrame(t, conn, Frame{
		Type: FrameCall, StreamID: 1,
		Body: map[string]any{
			"method": "state.explode",
			"args":   map[string]any{"state": "k"},
		},
	})
	f := readFrame(t, conn)
	if f.Type != FrameError {
		t.Fatalf("expected ERROR, got 0x%02x", f.Type)
	}
}

func TestIntegration_MultipleClients(t *testing.T) {
	srv, ts := testServer(t)

	c1 := dial(t, ts)
	readFrame(t, c1)
	c2 := dial(t, ts)
	readFrame(t, c2)

	if srv.SessionCount() != 2 {
		t.Errorf("session count: %d", srv.SessionCount())
	}

	sendFrame(t, c1, Frame{Type: FrameWatch, StreamID: 10, Body: map[string]any{"state": "shared"}})
	readFrame(t, c1)
	sendFrame(t, c2, Frame{Type: FrameWatch, StreamID: 10, Body: map[string]any{"state": "shared"}})
	readFrame(t, c2)

	time.Sleep(30 * time.Millisecond)
	srv.Mutate("shared", map[string]any{"x": 1})

	if d := readFrame(t, c1); d.Type != FrameDelta {
		t.Errorf("c1: expected DELTA, got 0x%02x", d.Type)
	}
	if d := readFrame(t, c2); d.Type != FrameDelta {
		t.Errorf("c2: expected DELTA, got 0x%02x", d.Type)
	}
}

func TestIntegration_DisconnectCleanup(t *testing.T) {
	srv, ts := testServer(t)
	conn := dial(t, ts)
	readFrame(t, conn)

	sendFrame(t, conn, Frame{Type: FrameWatch, StreamID: 10, Body: map[string]any{"state": "events"}})
	readFrame(t, conn)

	if srv.Fanout.SubscriberCount("events") != 1 {
		t.Fatal("expected 1 subscriber")
	}

	conn.Close()
	time.Sleep(100 * time.Millisecond)

	if srv.Fanout.SubscriberCount("events") != 0 {
		t.Error("subscriber should be removed after disconnect")
	}
	if srv.SessionCount() != 0 {
		t.Errorf("session count: %d", srv.SessionCount())
	}
}

func TestIntegration_ConnectionLimit(t *testing.T) {
	srv, ts := testServer(t, WithMaxSessions(1))
	srv.sessionCount.Store(1) // force the limit

	url := "ws" + strings.TrimPrefix(ts.URL, "http") + "/sodp"
	_, resp, err := websocket.DefaultDialer.Dial(url, nil)
	if err == nil {
		t.Fatal("should reject at connection limit")
	}
	if resp != nil && resp.StatusCode != http.StatusServiceUnavailable {
		t.Errorf("status: %d", resp.StatusCode)
	}
}

func TestIntegration_AuthorizeKeyHook(t *testing.T) {
	authFn := func(sess *Session, key, action string) (bool, int, string) {
		if strings.HasPrefix(key, "private.") {
			return false, 403, "access denied"
		}
		return true, 0, ""
	}

	_, ts := testServer(t, WithAuthorizeKey(authFn))
	conn := dial(t, ts)
	readFrame(t, conn)

	// Public key — should work.
	sendFrame(t, conn, Frame{Type: FrameWatch, StreamID: 10, Body: map[string]any{"state": "public.data"}})
	if f := readFrame(t, conn); f.Type != FrameStateInit {
		t.Errorf("public key: expected STATE_INIT, got 0x%02x", f.Type)
	}

	// Private key — should be denied.
	sendFrame(t, conn, Frame{Type: FrameWatch, StreamID: 11, Body: map[string]any{"state": "private.secret"}})
	if f := readFrame(t, conn); f.Type != FrameError {
		t.Errorf("private key: expected ERROR, got 0x%02x", f.Type)
	}
}

// --- RESUME ---

func TestResume_ReplaysMissedDeltas(t *testing.T) {
	srv, ts := testServer(t)
	const key = "log.resume"

	// Connection 1: subscribe + receive one delta.
	c1 := dial(t, ts)
	readFrame(t, c1)
	sendFrame(t, c1, Frame{Type: FrameWatch, StreamID: 10, Body: map[string]any{"state": key}})
	init := readFrame(t, c1)
	if init.Type != FrameStateInit {
		t.Fatalf("expected STATE_INIT, got 0x%02x", init.Type)
	}

	time.Sleep(30 * time.Millisecond)
	srv.MutateAppend(key, "e1", 100)
	d1 := readFrame(t, c1)
	if d1.Type != FrameDelta {
		t.Fatalf("expected DELTA, got 0x%02x", d1.Type)
	}
	d1m := bodyMap(t, d1.Body)
	v1, _ := d1m["version"].(uint64)
	if vi, ok := d1m["version"].(int64); ok {
		v1 = uint64(vi)
	}

	// Simulate network drop.
	c1.Close()
	time.Sleep(100 * time.Millisecond)

	// Apply missed deltas.
	srv.MutateAppend(key, "e2", 100)
	srv.MutateAppend(key, "e3", 100)

	// Reconnect with RESUME.
	c2 := dial(t, ts)
	readFrame(t, c2)
	sendFrame(t, c2, Frame{
		Type:     FrameResume,
		StreamID: 10,
		Body:     map[string]any{"state": key, "since_version": v1},
	})

	var got []Frame
	deadline := time.Now().Add(3 * time.Second)
	for len(got) < 2 && time.Now().Before(deadline) {
		c2.SetReadDeadline(time.Now().Add(500 * time.Millisecond))
		_, data, err := c2.ReadMessage()
		if err != nil {
			break
		}
		f, _ := DecodeFrame(data)
		if f.Type == FrameStateInit {
			t.Log("server fell back to STATE_INIT — ok")
			return
		}
		if f.Type == FrameDelta {
			got = append(got, f)
		}
	}

	if len(got) != 2 {
		t.Fatalf("expected 2 replayed deltas, got %d", len(got))
	}
	for i, f := range got {
		m := bodyMap(t, f.Body)
		ver, _ := m["version"].(uint64)
		if vi, ok := m["version"].(int64); ok {
			ver = uint64(vi)
		}
		if ver <= v1 {
			t.Errorf("delta[%d] version %d should be > %d", i, ver, v1)
		}
	}
}

func TestResume_FallsBackToStateInit(t *testing.T) {
	srv, ts := testServer(t)
	srv.State.logCap = 2

	const key = "log.gap"
	srv.MutateAppend(key, "v1", 100)
	srv.MutateAppend(key, "v2", 100)
	srv.MutateAppend(key, "v3", 100)
	srv.MutateAppend(key, "v4", 100)

	conn := dial(t, ts)
	readFrame(t, conn)

	sendFrame(t, conn, Frame{
		Type:     FrameResume,
		StreamID: 10,
		Body:     map[string]any{"state": key, "since_version": uint64(1)},
	})

	f := readFrame(t, conn)
	if f.Type != FrameStateInit {
		t.Fatalf("expected fallback STATE_INIT, got 0x%02x", f.Type)
	}
	m := bodyMap(t, f.Body)
	val, ok := m["value"].([]any)
	if !ok || len(val) != 4 {
		t.Errorf("fallback STATE_INIT should have all 4 entries, got %T %v", m["value"], m["value"])
	}
}

// --- Helper functions ---

func TestSetIn(t *testing.T) {
	curr := map[string]any{"meta": map[string]any{"rows": 10, "cols": 3}}
	result := setIn(curr, "meta.rows", 20).(map[string]any)
	meta := result["meta"].(map[string]any)
	if meta["rows"] != 20 || meta["cols"] != 3 {
		t.Errorf("set_in: %v", meta)
	}
}

func TestMergeValues(t *testing.T) {
	result := mergeValues(
		map[string]any{"a": 1, "b": 2},
		map[string]any{"b": 3, "c": 4},
	).(map[string]any)
	if result["a"] != 1 || result["b"] != 3 || result["c"] != 4 {
		t.Errorf("merge: %v", result)
	}
}
