// JWT verification for HS256 (shared secret) and RS256 (public key PEM).
// Uses `jose` — pure JS, works in Node.js, Bun, Deno, and edge runtimes.

import { jwtVerify, createRemoteJWKSet, importSPKI } from 'jose'
import { createSecretKey } from 'crypto'

export interface SodpClaims {
  sub: string
  roles: string[]
  groups: string[]
  perms: string[]
  exp?: number
  extra: Record<string, unknown>
}

export type JwtConfig =
  | { type: 'hs256'; secret: string }
  | { type: 'rs256'; publicKeyPem: string }

// Verify a raw JWT string. Returns parsed claims on success, throws on failure.
export async function verifyJwt(token: string, config: JwtConfig): Promise<SodpClaims> {
  let payload: Record<string, unknown>

  if (config.type === 'hs256') {
    const key = createSecretKey(Buffer.from(config.secret, 'utf-8'))
    const { payload: p } = await jwtVerify(token, key)
    payload = p as Record<string, unknown>
  } else {
    const key = await importSPKI(config.publicKeyPem, 'RS256')
    const { payload: p } = await jwtVerify(token, key)
    payload = p as Record<string, unknown>
  }

  const sub = typeof payload.sub === 'string' ? payload.sub : ''
  const exp = typeof payload.exp === 'number' ? payload.exp : undefined

  // Normalize roles/groups/perms — accept string[] or space-separated string
  const toArray = (v: unknown): string[] => {
    if (Array.isArray(v)) return v.filter((x): x is string => typeof x === 'string')
    if (typeof v === 'string') return v.split(' ').filter(Boolean)
    return []
  }

  const roles  = toArray(payload['roles']  ?? payload['role'])
  const groups = toArray(payload['groups'] ?? payload['group'])
  const perms  = toArray(payload['perms']  ?? payload['permissions'])

  const knownKeys = new Set(['sub', 'iss', 'aud', 'exp', 'iat', 'nbf',
                              'roles', 'role', 'groups', 'group', 'perms', 'permissions'])
  const extra: Record<string, unknown> = {}
  for (const [k, v] of Object.entries(payload)) {
    if (!knownKeys.has(k)) extra[k] = v
  }

  return { sub, roles, groups, perms, exp, extra }
}

// Build a JwtConfig from environment variables, matching the Rust server convention.
// Returns null if no auth env vars are set (auth disabled).
export function jwtConfigFromEnv(): JwtConfig | null {
  const secret = process.env['SODP_JWT_SECRET']
  const keyFile = process.env['SODP_JWT_PUBLIC_KEY_FILE']
  const keyInline = process.env['SODP_JWT_PUBLIC_KEY']

  if (keyFile) {
    const { readFileSync } = require('fs') as typeof import('fs')
    const pem = readFileSync(keyFile, 'utf-8')
    return { type: 'rs256', publicKeyPem: pem }
  }
  if (keyInline) {
    // Support \n-escaped PEM in env var
    return { type: 'rs256', publicKeyPem: keyInline.replace(/\\n/g, '\n') }
  }
  if (secret) {
    return { type: 'hs256', secret }
  }
  return null
}
