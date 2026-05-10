// SODP v1.0 wire framing.
//
// Every frame is a 4-tuple MessagePack array: [op, stream, seq, body].
// Clients use @sodp/client which talks this exact shape.
//
// Opcode ranges:
//   0x01–0x0D control/data frames (HELLO..UNWATCH)

import { encode, decode } from '@msgpack/msgpack'

export const OP = {
  HELLO:      0x01,
  WATCH:      0x02,
  STATE_INIT: 0x03,
  DELTA:      0x04,
  CALL:       0x05,
  RESULT:     0x06,
  ERROR:      0x07,
  HEARTBEAT:  0x09,
  RESUME:     0x0A,
  AUTH:       0x0B,
  AUTH_OK:    0x0C,
  UNWATCH:    0x0D,
} as const

export type Op = typeof OP[keyof typeof OP]

export interface Frame<TBody = unknown> {
  op: Op
  stream: number
  seq: number
  body: TBody
}

export interface HelloBody {
  protocol: 'sodp'
  version: '1.0'
  server: string
  auth: boolean
  capabilities: {
    rate_limit_writes: number
    rate_limit_watches: number
    backpressure_limit: number
    multi_watch: boolean
    params: boolean
    resume: boolean
  }
}

export interface StateInitBody {
  state: string
  version: number
  value: unknown
  initialized: boolean
  params?: Record<string, unknown>
}

export interface DeltaBody {
  version: number
  ops: DeltaOp[]
  state?: string
}

export interface DeltaOp {
  op: 'ADD' | 'UPDATE' | 'REMOVE'
  path: string
  value?: unknown
}

export interface ErrorBody {
  code: number
  message: string
}

export interface AuthOkBody {
  sub: string
}

export interface WatchBody {
  state?: string
  states?: string[]
  params?: Record<string, unknown>
}

export interface UnwatchBody {
  state?: string
  states?: string[]
}

export interface ResumeBody {
  state: string
  since_version: number
  params?: Record<string, unknown>
}

export interface AuthBody {
  token: string
}

export interface CallBody {
  call_id: string
  method: string
  args: Record<string, unknown>
}

function encodeFrame(op: Op, stream: number, seq: number, body: unknown): Uint8Array {
  return encode([op, stream, seq, body])
}

export const Frames = {
  hello(body: HelloBody): Uint8Array {
    return encodeFrame(OP.HELLO, 0, 0, body)
  },
  authOk(seq: number, body: AuthOkBody): Uint8Array {
    return encodeFrame(OP.AUTH_OK, 0, seq, body)
  },
  stateInit(stream: number, seq: number, body: StateInitBody): Uint8Array {
    return encodeFrame(OP.STATE_INIT, stream, seq, body)
  },
  delta(stream: number, seq: number, body: DeltaBody): Uint8Array {
    return encodeFrame(OP.DELTA, stream, seq, body)
  },
  error(stream: number, seq: number, body: ErrorBody): Uint8Array {
    return encodeFrame(OP.ERROR, stream, seq, body)
  },
  heartbeat(): Uint8Array {
    return encodeFrame(OP.HEARTBEAT, 0, 0, null)
  },
  result(seq: number, callId: string, success: boolean, data: unknown): Uint8Array {
    return encodeFrame(OP.RESULT, 0, seq, { call_id: callId, success, data })
  },
} as const

export function decodeFrame(buf: ArrayBufferLike | Uint8Array): Frame {
  const raw = decode(buf)
  if (!Array.isArray(raw) || raw.length !== 4) {
    throw new Error('malformed frame: not a 4-tuple')
  }
  const [op, stream, seq, body] = raw as [unknown, unknown, unknown, unknown]
  if (typeof op !== 'number' || typeof stream !== 'number' || typeof seq !== 'number') {
    throw new Error('malformed frame: op/stream/seq must be numbers')
  }
  return { op: op as Op, stream, seq, body }
}
