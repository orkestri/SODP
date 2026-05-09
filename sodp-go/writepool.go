package sodp

import (
	"runtime"
	"sync"
	"time"

	"github.com/gorilla/websocket"
)

// poolBufPool recycles byte slices for assembling outgoing DELTA frame headers.
var poolBufPool = sync.Pool{
	New: func() any { return make([]byte, 0, 512) },
}

// WritePool is a bounded set of goroutines that write WebSocket frames to
// client connections. Instead of one writePump goroutine per session (which
// causes a thundering-herd scheduler storm when N sessions all have frames
// queued simultaneously), a fixed number of workers service all sessions.
//
// Each session has its own Send channel (message queue). A CAS flag prevents
// the same session from being enqueued twice. Workers drain a session's entire
// queue in one pass (write coalescing) before releasing the per-connection
// write mutex and marking it available again.
type WritePool struct {
	queue     chan *Session
	writeWait time.Duration
	done      chan struct{}
	once      sync.Once
	coll      Collector
}

// NewWritePool starts GOMAXPROCS×2 worker goroutines.
func NewWritePool(writeWait time.Duration, coll Collector) *WritePool {
	if coll == nil {
		coll = noopCollector{}
	}
	workers := runtime.GOMAXPROCS(0) * 2
	p := &WritePool{
		queue:     make(chan *Session, workers*1024),
		writeWait: writeWait,
		done:      make(chan struct{}),
		coll:      coll,
	}
	for i := 0; i < workers; i++ {
		go p.worker()
	}
	return p
}

// Close stops all worker goroutines.
func (p *WritePool) Close() {
	p.once.Do(func() { close(p.done) })
}

// Send queues msg for sess and schedules sess for draining.
// If sess.Send is full the message is silently dropped (backpressure).
func (p *WritePool) Send(sess *Session, msg outMsg) {
	select {
	case sess.Send <- msg:
	default:
		p.coll.DeltaDropped("pool")
		return
	}
	p.enqueue(sess)
}

// enqueue adds sess to the pool queue if it is not already there.
func (p *WritePool) enqueue(sess *Session) {
	if sess.inPool.CompareAndSwap(false, true) {
		select {
		case p.queue <- sess:
		default:
			sess.inPool.Store(false)
		}
	}
}

func (p *WritePool) worker() {
	for {
		select {
		case sess := <-p.queue:
			p.drain(sess)
		case <-p.done:
			return
		}
	}
}

// drain writes all messages currently buffered for sess in a single pass
// (write coalescing). Per-connection serialization is maintained by writeMu.
// After releasing the mutex, it rechecks for newly arrived messages to close
// the CAS race window.
func (p *WritePool) drain(sess *Session) {
	conn := sess.Conn
	sess.writeMu.Lock()

loop:
	for {
		select {
		case msg := <-sess.Send:
			conn.SetWriteDeadline(time.Now().Add(p.writeWait))
			var err error
			if msg.raw != nil {
				err = conn.WriteMessage(websocket.BinaryMessage, msg.raw)
			} else {
				// Assemble DELTA: [fixarray(4), FrameDelta, stream_id, seq+body...]
				buf := poolBufPool.Get().([]byte)[:0]
				buf = append(buf, 0x94, byte(FrameDelta))
				buf = appendMsgpackUint(buf, uint64(msg.streamID))
				buf = append(buf, msg.tail...)
				err = conn.WriteMessage(websocket.BinaryMessage, buf)
				poolBufPool.Put(buf) // gorilla copies before returning
			}
			if err != nil {
				break loop
			}
		default:
			break loop
		}
	}

	sess.writeMu.Unlock()
	sess.inPool.Store(false)
	// Close the race window: if a producer wrote to sess.Send between the last
	// drain iteration and the Store above, re-enqueue so it gets written.
	if len(sess.Send) > 0 {
		p.enqueue(sess)
	}
}
