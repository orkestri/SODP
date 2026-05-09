// Package sodp implements the State-Oriented Data Protocol (SODP) v1.0
// as an embeddable Go library for real-time state synchronization over WebSockets.
//
// Wire format: each message is a 4-element MessagePack array:
//
//	[frame_type(u8), stream_id(u32), seq(u64), body(any)]
//
// Quickstart:
//
//	srv := sodp.NewServer()
//	http.HandleFunc("/sodp", srv.HandleWS)
//	http.ListenAndServe(":7777", nil)
package sodp

import (
	"fmt"

	"github.com/vmihailenco/msgpack/v5"
)

// FrameType identifies the SODP message type (wire: u8).
type FrameType uint8

const (
	FrameHello     FrameType = 0x01
	FrameWatch     FrameType = 0x02
	FrameStateInit FrameType = 0x03
	FrameDelta     FrameType = 0x04
	FrameCall      FrameType = 0x05
	FrameResult    FrameType = 0x06
	FrameError     FrameType = 0x07
	FrameHeartbeat FrameType = 0x09
	FrameResume    FrameType = 0x0A
	FrameAuth      FrameType = 0x0B
	FrameAuthOK    FrameType = 0x0C
	FrameUnwatch   FrameType = 0x0D
)

// Frame is a decoded SODP wire message.
type Frame struct {
	Type     FrameType
	StreamID uint32
	Seq      uint64
	Body     any
}

// EncodeFrame serializes a Frame into MessagePack bytes.
func EncodeFrame(f Frame) ([]byte, error) {
	return msgpack.Marshal([]any{uint8(f.Type), f.StreamID, f.Seq, f.Body})
}

// DecodeFrame deserializes MessagePack bytes into a Frame.
func DecodeFrame(data []byte) (Frame, error) {
	var raw []msgpack.RawMessage
	if err := msgpack.Unmarshal(data, &raw); err != nil {
		return Frame{}, fmt.Errorf("sodp: invalid frame: %w", err)
	}
	if len(raw) != 4 {
		return Frame{}, fmt.Errorf("sodp: frame must have 4 elements, got %d", len(raw))
	}

	var ft uint8
	if err := msgpack.Unmarshal(raw[0], &ft); err != nil {
		return Frame{}, fmt.Errorf("sodp: invalid frame_type: %w", err)
	}
	var streamID uint32
	if err := msgpack.Unmarshal(raw[1], &streamID); err != nil {
		return Frame{}, fmt.Errorf("sodp: invalid stream_id: %w", err)
	}
	var seq uint64
	if err := msgpack.Unmarshal(raw[2], &seq); err != nil {
		return Frame{}, fmt.Errorf("sodp: invalid seq: %w", err)
	}

	var body any
	if len(raw[3]) > 0 {
		if err := msgpack.Unmarshal(raw[3], &body); err != nil {
			return Frame{}, fmt.Errorf("sodp: invalid body: %w", err)
		}
	}

	return Frame{
		Type:     FrameType(ft),
		StreamID: streamID,
		Seq:      seq,
		Body:     body,
	}, nil
}

// HelloBody is the server's initial handshake payload.
type HelloBody struct {
	Protocol     string         `msgpack:"protocol"`
	Version      string         `msgpack:"version"`
	ServerID     string         `msgpack:"server_id"`
	Auth         bool           `msgpack:"auth"`
	Capabilities map[string]any `msgpack:"capabilities,omitempty"`
}

// WatchBody is the client's subscription request (WATCH and RESUME).
type WatchBody struct {
	Key          string         `msgpack:"state"`
	States       []string       `msgpack:"states,omitempty"`
	SinceVersion uint64         `msgpack:"since_version,omitempty"`
	Params       map[string]any `msgpack:"params,omitempty"`
}

// CallBody is the CALL frame payload.
type CallBody struct {
	CallID string         `msgpack:"call_id"`
	Method string         `msgpack:"method"`
	Args   map[string]any `msgpack:"args"`
}

// ErrorBody carries structured error details.
type ErrorBody struct {
	Code    int    `msgpack:"code"`
	Message string `msgpack:"message"`
}

// AuthBody carries the JWT token from the client.
type AuthBody struct {
	Token string `msgpack:"token"`
}

// AuthOKBody confirms authentication success.
type AuthOKBody struct {
	Subject string `msgpack:"sub"`
}
