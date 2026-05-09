package sodp

import (
	"crypto/rsa"
	"crypto/x509"
	"encoding/pem"
	"fmt"
	"log"
	"net/http"
	"strings"
	"sync"
	"sync/atomic"
	"time"
	"unicode/utf8"

	"github.com/golang-jwt/jwt/v5"
	"github.com/google/uuid"
	"github.com/gorilla/websocket"
)

const (
	defaultPingInterval    = 30 * time.Second
	defaultPongWait        = 60 * time.Second
	defaultWriteWait       = 10 * time.Second
	defaultMaxFrameBytes   = 64 * 1024
	defaultMaxKeyLen       = 256
	defaultMaxValueBytes   = 512 * 1024
	defaultMaxSessions     = 4096
	defaultBackpressure    = 128
)

// AuthorizeKeyFn is called before every WATCH, RESUME, and state-mutating CALL.
// Return (true, _, _) to allow, or (false, code, message) to deny.
// If nil, all key access is allowed.
type AuthorizeKeyFn func(sess *Session, key string) (allowed bool, code int, message string)

// jwtConfig holds either an HS256 secret or an RS256 public key.
type jwtConfig struct {
	hs256Secret   []byte
	rs256PubKey   *rsa.PublicKey
}

func (c *jwtConfig) enabled() bool {
	return c != nil && (len(c.hs256Secret) > 0 || c.rs256PubKey != nil)
}

// options holds all configurable Server parameters.
type options struct {
	checkOrigin      func(*http.Request) bool
	jwt              *jwtConfig
	requireAuth      bool
	authorizeKey     AuthorizeKeyFn
	maxSessions      int
	rateLimit        int
	maxWatches       int
	backpressure     int
	pingInterval     time.Duration
	pongWait         time.Duration
	writeWait        time.Duration
	maxFrameBytes    int64
}

// ServerOption configures a Server.
type ServerOption func(*options)

// WithAllowedOrigins configures CORS. Pass nil to allow all origins.
// Pass a list of allowed origin strings for strict validation.
func WithAllowedOrigins(origins []string) ServerOption {
	return func(o *options) {
		if len(origins) == 0 {
			o.checkOrigin = func(*http.Request) bool { return true }
			return
		}
		set := make(map[string]struct{}, len(origins))
		for _, orig := range origins {
			set[orig] = struct{}{}
		}
		o.checkOrigin = func(r *http.Request) bool {
			orig := r.Header.Get("Origin")
			if orig == "" {
				return true
			}
			_, ok := set[orig]
			return ok
		}
	}
}

// WithCheckOrigin sets a custom origin check function.
func WithCheckOrigin(fn func(*http.Request) bool) ServerOption {
	return func(o *options) { o.checkOrigin = fn }
}

// WithJWTSecret configures HS256 JWT validation using a shared secret.
// Enables authentication (clients must send an AUTH frame).
func WithJWTSecret(secret []byte) ServerOption {
	return func(o *options) {
		if o.jwt == nil {
			o.jwt = &jwtConfig{}
		}
		o.jwt.hs256Secret = secret
		o.requireAuth = true
	}
}

// WithJWTPublicKey configures RS256 JWT validation using a PEM-encoded RSA public key.
// RS256 takes priority over HS256 if both are configured.
func WithJWTPublicKey(pemKey string) ServerOption {
	return func(o *options) {
		block, _ := pem.Decode([]byte(pemKey))
		if block == nil {
			log.Printf("sodp: WithJWTPublicKey: invalid PEM block — auth disabled")
			return
		}
		pub, err := x509.ParsePKIXPublicKey(block.Bytes)
		if err != nil {
			log.Printf("sodp: WithJWTPublicKey: parse error: %v — auth disabled", err)
			return
		}
		rsaPub, ok := pub.(*rsa.PublicKey)
		if !ok {
			log.Printf("sodp: WithJWTPublicKey: not an RSA key — auth disabled")
			return
		}
		if o.jwt == nil {
			o.jwt = &jwtConfig{}
		}
		o.jwt.rs256PubKey = rsaPub
		o.requireAuth = true
	}
}

// WithAuthorizeKey sets a key-level authorization hook called before every
// WATCH, RESUME, and CALL. Return (false, code, message) to deny access.
func WithAuthorizeKey(fn AuthorizeKeyFn) ServerOption {
	return func(o *options) { o.authorizeKey = fn }
}

// WithMaxSessions sets the hard connection limit. Default: 4096.
func WithMaxSessions(n int) ServerOption {
	return func(o *options) { o.maxSessions = n }
}

// WithRateLimit sets the per-session write mutation rate limit (calls/sec). Default: 100.
func WithRateLimit(n int) ServerOption {
	return func(o *options) { o.rateLimit = n }
}

// WithMaxWatches sets the maximum concurrent WATCH subscriptions per session. Default: 64.
func WithMaxWatches(n int) ServerOption {
	return func(o *options) { o.maxWatches = n }
}

// WithBackpressureLimit sets the per-session outbound channel buffer size.
// When full, slow-client deltas are dropped. Default: 128.
func WithBackpressureLimit(n int) ServerOption {
	return func(o *options) { o.backpressure = n }
}

// Server is the SODP WebSocket server. It owns the state store and fanout bus,
// and manages client sessions. Embed or wrap Server in your HTTP handler stack.
//
//	srv := sodp.NewServer(sodp.WithJWTSecret([]byte("secret")))
//	http.HandleFunc("/sodp", srv.HandleWS)
type Server struct {
	State  *StateStore
	Fanout *FanoutBus

	mu           sync.RWMutex
	sessions     map[string]*Session
	sessionCount atomic.Int64

	serverID string
	opts     options
	upgrader websocket.Upgrader
}

// NewServer creates a new SODP server instance with the provided options.
func NewServer(optFns ...ServerOption) *Server {
	o := options{
		maxSessions:  defaultMaxSessions,
		rateLimit:    defaultRateLimit,
		maxWatches:   defaultMaxWatches,
		backpressure: defaultBackpressure,
		pingInterval: defaultPingInterval,
		pongWait:     defaultPongWait,
		writeWait:    defaultWriteWait,
		maxFrameBytes: defaultMaxFrameBytes,
	}
	o.checkOrigin = func(*http.Request) bool { return true }

	for _, fn := range optFns {
		fn(&o)
	}

	srv := &Server{
		State:    NewStateStore(),
		Fanout:   NewFanoutBus(),
		sessions: make(map[string]*Session),
		serverID: uuid.New().String()[:8],
		opts:     o,
	}
	srv.upgrader = websocket.Upgrader{
		CheckOrigin: o.checkOrigin,
	}
	return srv
}

// HandleWS is the http.HandlerFunc for WebSocket upgrade.
// Register it with any HTTP mux:
//
//	mux.HandleFunc("/sodp", srv.HandleWS)
func (srv *Server) HandleWS(w http.ResponseWriter, r *http.Request) {
	if srv.sessionCount.Load() >= int64(srv.opts.maxSessions) {
		http.Error(w, "too many connections", http.StatusServiceUnavailable)
		return
	}

	conn, err := srv.upgrader.Upgrade(w, r, nil)
	if err != nil {
		log.Printf("sodp: websocket upgrade: %v", err)
		return
	}
	conn.SetReadLimit(srv.opts.maxFrameBytes)

	sess := NewSession(uuid.New().String())
	sess.rateLimit = srv.opts.rateLimit
	sess.maxWatches = srv.opts.maxWatches
	sess.Send = make(chan []byte, srv.opts.backpressure)

	srv.mu.Lock()
	srv.sessions[sess.ID] = sess
	srv.mu.Unlock()
	srv.sessionCount.Add(1)

	conn.SetReadDeadline(time.Now().Add(srv.opts.pongWait))
	conn.SetPongHandler(func(string) error {
		conn.SetReadDeadline(time.Now().Add(srv.opts.pongWait))
		return nil
	})

	// Send HELLO before starting the write pump — the only direct write.
	caps := map[string]any{
		"max_sessions":    srv.opts.maxSessions,
		"max_watches":     srv.opts.maxWatches,
		"rate_limit":      srv.opts.rateLimit,
		"backpressure":    srv.opts.backpressure,
	}
	if srv.opts.jwt.enabled() {
		if srv.opts.jwt.rs256PubKey != nil {
			caps["jwt"] = "rs256"
		} else {
			caps["jwt"] = "hs256"
		}
	}
	if data, err := EncodeFrame(Frame{
		Type: FrameHello,
		Body: HelloBody{
			Protocol:     "sodp",
			Version:      "1.0",
			ServerID:     srv.serverID,
			Auth:         srv.opts.requireAuth,
			Capabilities: caps,
		},
	}); err == nil {
		conn.SetWriteDeadline(time.Now().Add(srv.opts.writeWait))
		conn.WriteMessage(websocket.BinaryMessage, data)
	}

	go srv.writePump(conn, sess)
	srv.readPump(conn, sess)

	sess.Close()
	srv.Fanout.RemoveSession(sess.ID)
	srv.mu.Lock()
	delete(srv.sessions, sess.ID)
	srv.mu.Unlock()
	srv.sessionCount.Add(-1)
	conn.Close()
}

func (srv *Server) writePump(conn *websocket.Conn, sess *Session) {
	ticker := time.NewTicker(srv.opts.pingInterval)
	defer ticker.Stop()

	for {
		select {
		case msg, ok := <-sess.Send:
			if !ok {
				conn.WriteMessage(websocket.CloseMessage, []byte{})
				return
			}
			conn.SetWriteDeadline(time.Now().Add(srv.opts.writeWait))
			if err := conn.WriteMessage(websocket.BinaryMessage, msg); err != nil {
				return
			}
			// drain queued messages in the same write cycle
			for n := len(sess.Send); n > 0; n-- {
				if err := conn.WriteMessage(websocket.BinaryMessage, <-sess.Send); err != nil {
					return
				}
			}
		case <-ticker.C:
			conn.SetWriteDeadline(time.Now().Add(srv.opts.writeWait))
			if err := conn.WriteMessage(websocket.PingMessage, nil); err != nil {
				return
			}
		case <-sess.Done:
			return
		}
	}
}

func (srv *Server) readPump(conn *websocket.Conn, sess *Session) {
	for {
		_, data, err := conn.ReadMessage()
		if err != nil {
			return
		}
		f, err := DecodeFrame(data)
		if err != nil {
			srv.sendError(sess, 0, 400, "invalid frame")
			continue
		}
		srv.dispatch(sess, f)
	}
}

func (srv *Server) dispatch(sess *Session, f Frame) {
	switch f.Type {
	case FrameAuth:
		srv.handleAuth(sess, f)
	case FrameWatch:
		if srv.checkAuth(sess) {
			srv.handleWatch(sess, f)
		}
	case FrameUnwatch:
		srv.handleUnwatch(sess, f)
	case FrameCall:
		if srv.checkAuth(sess) {
			srv.handleCall(sess, f)
		}
	case FrameResume:
		if srv.checkAuth(sess) {
			srv.handleResume(sess, f)
		}
	case FrameHeartbeat:
		srv.sendFrame(sess, Frame{Type: FrameHeartbeat, Seq: f.Seq})
	default:
		srv.sendError(sess, f.StreamID, 400, "unknown frame type")
	}
}

// checkAuth returns false and sends ERROR 401 if auth is required but missing.
func (srv *Server) checkAuth(sess *Session) bool {
	if !srv.opts.requireAuth || sess.IsAuthenticated() {
		return true
	}
	srv.sendError(sess, 0, 401, "authentication required")
	return false
}

func (srv *Server) handleAuth(sess *Session, f Frame) {
	if sess.IsAuthenticated() {
		srv.sendError(sess, 0, 400, "already authenticated")
		return
	}

	if !srv.opts.jwt.enabled() {
		sess.MarkAuthenticated()
		srv.sendFrame(sess, Frame{Type: FrameAuthOK, Body: AuthOKBody{Subject: "anonymous"}})
		return
	}

	body, err := decodeAuthBody(f.Body)
	if err != nil {
		srv.sendError(sess, 0, 400, "invalid auth body")
		return
	}

	sub, claims, err := srv.validateJWT(body.Token)
	if err != nil {
		srv.sendError(sess, 0, 401, "invalid token")
		return
	}

	sess.SetAuth(sub, claims)
	srv.sendFrame(sess, Frame{Type: FrameAuthOK, Body: AuthOKBody{Subject: sub}})
}

func (srv *Server) validateJWT(tokenStr string) (sub string, claims map[string]any, err error) {
	cfg := srv.opts.jwt

	var keyFunc jwt.Keyfunc
	if cfg.rs256PubKey != nil {
		keyFunc = func(t *jwt.Token) (any, error) {
			if _, ok := t.Method.(*jwt.SigningMethodRSA); !ok {
				return nil, fmt.Errorf("expected RS256")
			}
			return cfg.rs256PubKey, nil
		}
	} else {
		keyFunc = func(t *jwt.Token) (any, error) {
			if _, ok := t.Method.(*jwt.SigningMethodHMAC); !ok {
				return nil, fmt.Errorf("expected HS256")
			}
			return cfg.hs256Secret, nil
		}
	}

	token, err := jwt.Parse(tokenStr, keyFunc)
	if err != nil || !token.Valid {
		return "", nil, fmt.Errorf("invalid token")
	}

	mc, ok := token.Claims.(jwt.MapClaims)
	if !ok {
		return "", nil, fmt.Errorf("invalid claims")
	}

	sub, _ = mc["sub"].(string)
	claimMap := make(map[string]any, len(mc))
	for k, v := range mc {
		claimMap[k] = v
	}
	return sub, claimMap, nil
}

func (srv *Server) handleWatch(sess *Session, f Frame) {
	body, err := decodeWatchBody(f.Body)
	if err != nil {
		srv.sendError(sess, f.StreamID, 400, "invalid watch body")
		return
	}

	// Multi-key WATCH: decompose into individual subscriptions.
	keys := body.States
	if len(keys) == 0 && body.Key != "" {
		keys = []string{body.Key}
	}
	if len(keys) == 0 {
		srv.sendError(sess, f.StreamID, 400, "missing state key")
		return
	}

	for i, key := range keys {
		if !isValidKey(key) {
			srv.sendError(sess, f.StreamID, 400, "invalid key: "+key)
			return
		}
		if err := srv.authorizeKey(sess, f.StreamID, key); err != nil {
			return
		}

		streamID := f.StreamID
		if len(keys) > 1 || streamID == 0 {
			if i == 0 && streamID != 0 {
				// first key reuses the provided stream_id
			} else {
				streamID = sess.AllocStream()
			}
		}

		if !sess.AddWatch(streamID, key) {
			srv.sendError(sess, f.StreamID, 429, "watch limit reached")
			return
		}

		srv.Fanout.Subscribe(key, Subscriber{
			SessionID: sess.ID,
			StreamID:  streamID,
			Send:      sess.Send,
		})

		val, ver := srv.State.Get(key)
		srv.sendFrame(sess, Frame{
			Type:     FrameStateInit,
			StreamID: streamID,
			Body: map[string]any{
				"state":       key,
				"version":     ver,
				"value":       val,
				"initialized": val != nil,
				"params":      body.Params,
			},
		})
	}
}

func (srv *Server) handleUnwatch(sess *Session, f Frame) {
	body, _ := f.Body.(map[string]any)

	// Support both stream_id-based and key-based UNWATCH.
	if f.StreamID != 0 {
		key, ok := sess.RemoveWatch(f.StreamID)
		if ok {
			srv.Fanout.Unsubscribe(key, sess.ID, f.StreamID)
		}
		return
	}

	// Key-based UNWATCH (single key or states array).
	var keys []string
	if body != nil {
		if k, ok := body["state"].(string); ok && k != "" {
			keys = []string{k}
		}
		if arr, ok := body["states"].([]any); ok {
			for _, v := range arr {
				if k, ok := v.(string); ok {
					keys = append(keys, k)
				}
			}
		}
	}
	for _, key := range keys {
		if sid := sess.StreamForKey(key); sid != 0 {
			sess.RemoveWatch(sid)
			srv.Fanout.Unsubscribe(key, sess.ID, sid)
		}
	}
}

func (srv *Server) handleCall(sess *Session, f Frame) {
	if !sess.CheckRate() {
		srv.sendError(sess, f.StreamID, 429, "rate limit exceeded")
		return
	}

	body, err := decodeCallBody(f.Body)
	if err != nil {
		srv.sendError(sess, f.StreamID, 400, "invalid call body")
		return
	}

	key, _ := body.Args["state"].(string)
	if key == "" {
		srv.sendError(sess, f.StreamID, 400, "missing state key in args")
		return
	}
	if !isValidKey(key) {
		srv.sendError(sess, f.StreamID, 400, "invalid key")
		return
	}
	if err := srv.authorizeKey(sess, f.StreamID, key); err != nil {
		return
	}

	var delta *DeltaEntry

	switch body.Method {
	case "state.set":
		delta = srv.State.Apply(key, body.Args["value"])
	case "state.patch":
		current, _ := srv.State.Get(key)
		delta = srv.State.Apply(key, mergeValues(current, body.Args["patch"]))
	case "state.set_in":
		path, _ := body.Args["path"].(string)
		if path == "" {
			srv.sendError(sess, f.StreamID, 400, "path required for state.set_in")
			return
		}
		current, _ := srv.State.Get(key)
		delta = srv.State.Apply(key, setIn(current, path, body.Args["value"]))
	case "state.delete":
		delta = srv.State.Delete(key)
	case "state.append":
		var maxLen int
		if ml, ok := body.Args["max_len"].(int64); ok {
			maxLen = int(ml)
		} else if ml, ok := body.Args["max_len"].(float64); ok {
			maxLen = int(ml)
		}
		delta = srv.State.Append(key, body.Args["value"], maxLen)
	default:
		srv.sendError(sess, f.StreamID, 400, "unknown method: "+body.Method)
		return
	}

	resultData := map[string]any{}
	if delta != nil {
		resultData["version"] = delta.Version
	}
	srv.sendFrame(sess, Frame{
		Type:     FrameResult,
		StreamID: f.StreamID,
		Seq:      f.Seq,
		Body: map[string]any{
			"call_id": body.CallID,
			"success": true,
			"data":    resultData,
		},
	})

	if delta != nil {
		srv.sendDeltaDirect(sess, f.StreamID, delta)
		srv.Fanout.Broadcast(*delta, sess.ID)
	}
}

func (srv *Server) handleResume(sess *Session, f Frame) {
	body, err := decodeWatchBody(f.Body)
	if err != nil {
		srv.sendError(sess, f.StreamID, 400, "invalid resume body")
		return
	}
	if !isValidKey(body.Key) {
		srv.sendError(sess, f.StreamID, 400, "invalid key")
		return
	}
	if err := srv.authorizeKey(sess, f.StreamID, body.Key); err != nil {
		return
	}

	deltas := srv.State.DeltasSince(body.Key, body.SinceVersion)
	if deltas == nil {
		// Delta log doesn't cover the gap — fall back to full STATE_INIT.
		srv.handleWatch(sess, f)
		return
	}

	streamID := f.StreamID
	if streamID == 0 {
		streamID = sess.AllocStream()
	}
	if !sess.AddWatch(streamID, body.Key) {
		srv.sendError(sess, f.StreamID, 429, "watch limit reached")
		return
	}
	srv.Fanout.Subscribe(body.Key, Subscriber{
		SessionID: sess.ID,
		StreamID:  streamID,
		Send:      sess.Send,
	})

	for _, d := range deltas {
		frame, err := EncodeFrame(Frame{
			Type:     FrameDelta,
			StreamID: streamID,
			Seq:      d.Version,
			Body:     d,
		})
		if err != nil {
			continue
		}
		select {
		case sess.Send <- frame:
		default:
		}
	}
}

// sendDeltaDirect writes a DELTA frame directly to the session's outbound
// channel for the mutating session itself (avoids a fanout channel hop).
func (srv *Server) sendDeltaDirect(sess *Session, callStreamID uint32, delta *DeltaEntry) {
	watchStreamID := sess.StreamForKey(delta.Key)
	if watchStreamID == 0 {
		return // session is not watching this key
	}
	frame, err := EncodeFrame(Frame{
		Type:     FrameDelta,
		StreamID: watchStreamID,
		Seq:      delta.Version,
		Body:     delta,
	})
	if err != nil {
		return
	}
	select {
	case sess.Send <- frame:
	default:
	}
}

// Mutate applies a value change from server-side code (e.g., a background
// worker) and broadcasts the delta to all watchers.
func (srv *Server) Mutate(key string, value any) {
	delta := srv.State.Apply(key, value)
	if delta != nil {
		srv.Fanout.Broadcast(*delta, "")
	}
}

// MutateAppend appends an element to a slice-typed state key and broadcasts
// an O(1) ADD delta. maxLen ≤ 0 disables trimming.
func (srv *Server) MutateAppend(key string, element any, maxLen int) {
	delta := srv.State.Append(key, element, maxLen)
	if delta != nil {
		srv.Fanout.Broadcast(*delta, "")
	}
}

// MutateDelete removes a key and broadcasts a REMOVE delta.
func (srv *Server) MutateDelete(key string) {
	delta := srv.State.Delete(key)
	if delta != nil {
		srv.Fanout.Broadcast(*delta, "")
	}
}

// SessionCount returns the number of active sessions.
func (srv *Server) SessionCount() int {
	return int(srv.sessionCount.Load())
}

// --- security helpers ---

func (srv *Server) authorizeKey(sess *Session, streamID uint32, key string) error {
	if srv.opts.authorizeKey == nil {
		return nil
	}
	allowed, code, msg := srv.opts.authorizeKey(sess, key)
	if !allowed {
		srv.sendError(sess, streamID, code, msg)
		return fmt.Errorf("denied")
	}
	return nil
}

func isValidKey(key string) bool {
	if key == "" || len(key) > defaultMaxKeyLen {
		return false
	}
	if !utf8.ValidString(key) {
		return false
	}
	for _, r := range key {
		if r < 0x20 || r == '\\' || r == '/' {
			return false
		}
	}
	if key[0] == '.' || key[len(key)-1] == '.' {
		return false
	}
	return !strings.Contains(key, "..")
}

// --- frame helpers ---

func (srv *Server) sendFrame(sess *Session, f Frame) {
	data, err := EncodeFrame(f)
	if err != nil {
		return
	}
	select {
	case sess.Send <- data:
	default:
	}
}

func (srv *Server) sendError(sess *Session, streamID uint32, code int, msg string) {
	srv.sendFrame(sess, Frame{
		Type:     FrameError,
		StreamID: streamID,
		Body:     ErrorBody{Code: code, Message: msg},
	})
}

// --- body decoders ---

func decodeAuthBody(body any) (AuthBody, error) {
	m, ok := body.(map[string]any)
	if !ok {
		return AuthBody{}, fmt.Errorf("expected map")
	}
	token, _ := m["token"].(string)
	if token == "" {
		return AuthBody{}, fmt.Errorf("missing token")
	}
	return AuthBody{Token: token}, nil
}

func decodeWatchBody(body any) (WatchBody, error) {
	m, ok := body.(map[string]any)
	if !ok {
		return WatchBody{}, fmt.Errorf("expected map")
	}

	wb := WatchBody{}
	wb.Key, _ = m["state"].(string)

	if arr, ok := m["states"].([]any); ok {
		for _, v := range arr {
			if k, ok := v.(string); ok {
				wb.States = append(wb.States, k)
			}
		}
	}

	if wb.Key == "" && len(wb.States) == 0 {
		return WatchBody{}, fmt.Errorf("missing state")
	}

	switch v := m["since_version"].(type) {
	case uint64:
		wb.SinceVersion = v
	case int64:
		wb.SinceVersion = uint64(v)
	case float64:
		wb.SinceVersion = uint64(v)
	}

	if p, ok := m["params"].(map[string]any); ok {
		wb.Params = p
	}

	return wb, nil
}

func decodeCallBody(body any) (CallBody, error) {
	m, ok := body.(map[string]any)
	if !ok {
		return CallBody{}, fmt.Errorf("expected map")
	}
	method, _ := m["method"].(string)
	if method == "" {
		return CallBody{}, fmt.Errorf("missing method")
	}
	callID, _ := m["call_id"].(string)
	args, _ := m["args"].(map[string]any)
	if args == nil {
		args = make(map[string]any)
	}
	return CallBody{CallID: callID, Method: method, Args: args}, nil
}

// --- value helpers ---

// mergeValues performs a shallow JSON merge of patch into current.
func mergeValues(current, patch any) any {
	cm, cOK := toStringMap(current)
	pm, pOK := toStringMap(patch)
	if !cOK || !pOK {
		return patch
	}
	merged := make(map[string]any, len(cm)+len(pm))
	for k, v := range cm {
		merged[k] = v
	}
	for k, v := range pm {
		merged[k] = v
	}
	return merged
}

// setIn sets a nested field using a dot-separated path, returning a new map.
func setIn(current any, path string, value any) any {
	cm, ok := toStringMap(current)
	if !ok {
		cm = make(map[string]any)
	}
	result := make(map[string]any, len(cm))
	for k, v := range cm {
		result[k] = v
	}

	parts := strings.Split(path, ".")
	target := result
	for i, p := range parts {
		if i == len(parts)-1 {
			target[p] = value
			break
		}
		next, ok := toStringMap(target[p])
		if !ok {
			next = make(map[string]any)
		} else {
			cp := make(map[string]any, len(next))
			for k, v := range next {
				cp[k] = v
			}
			next = cp
		}
		target[p] = next
		target = next
	}
	return result
}
