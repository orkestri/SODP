// JWT verification for HS256 (shared secret) and RS256 (public key PEM).
// Uses `jose` — pure JS, works in Node.js, Bun, Deno, and edge runtimes.
//
// IdP presets map provider-specific claim paths to the standard
// { roles, groups, perms } fields so the ACL layer stays provider-agnostic.

import { jwtVerify, importSPKI } from 'jose'
import { readFileSync } from 'fs'
import { createSecretKey } from 'crypto'

export interface SodpClaims {
  sub: string
  roles: string[]
  groups: string[]
  perms: string[]
  exp?: number
  extra: Record<string, unknown>
}

// Supported IdP presets. Each preset defines where roles/groups/perms live in
// the raw JWT payload for that provider.
export type JwtPreset = 'keycloak' | 'auth0' | 'okta' | 'cognito' | 'generic'

export type JwtConfig =
  | { type: 'hs256'; secret: string; preset?: JwtPreset; claimMappings?: ClaimMappings }
  | { type: 'rs256'; publicKeyPem: string; preset?: JwtPreset; claimMappings?: ClaimMappings }

// Override individual claim paths. Keys: 'roles' | 'groups' | 'perms'.
// Values: dot-separated path into the JWT payload (e.g. 'realm_access.roles').
export interface ClaimMappings {
  roles?: string
  groups?: string
  perms?: string
}

// ── IdP presets ────────────────────────────────────────────────────────────────

interface PresetDef {
  roles: string    // dot-path into payload
  groups: string
  perms: string
}

const PRESETS: Record<JwtPreset, PresetDef> = {
  // Keycloak: roles in realm_access.roles, groups in groups claim
  keycloak: { roles: 'realm_access.roles', groups: 'groups', perms: 'scope' },
  // Auth0: roles/groups under custom namespace claims
  auth0:    { roles: 'https://sodp.io/roles', groups: 'https://sodp.io/groups', perms: 'permissions' },
  // Okta: groups claim carries roles, scp for permissions
  okta:     { roles: 'groups', groups: 'groups', perms: 'scp' },
  // Cognito: groups in cognito:groups, no native roles
  cognito:  { roles: 'cognito:groups', groups: 'cognito:groups', perms: 'scope' },
  // Generic: standard fields, same as default extraction
  generic:  { roles: 'roles', groups: 'groups', perms: 'permissions' },
}

// Resolve a dot-path like 'realm_access.roles' against a payload.
function getPath(payload: Record<string, unknown>, path: string): unknown {
  const parts = path.split('.')
  let v: unknown = payload
  for (const p of parts) {
    if (v === null || typeof v !== 'object') return undefined
    v = (v as Record<string, unknown>)[p]
  }
  return v
}

function toArray(v: unknown): string[] {
  if (Array.isArray(v)) return v.filter((x): x is string => typeof x === 'string')
  if (typeof v === 'string') return v.split(' ').filter(Boolean)
  return []
}

function extractClaims(
  payload: Record<string, unknown>,
  preset?: JwtPreset,
  overrides?: ClaimMappings,
): { roles: string[]; groups: string[]; perms: string[] } {
  const base = preset ? PRESETS[preset] : PRESETS.generic

  const rolePath   = overrides?.roles  ?? base.roles
  const groupPath  = overrides?.groups ?? base.groups
  const permPath   = overrides?.perms  ?? base.perms

  // For preset paths, use dot-traversal; for the generic case also try direct keys.
  const roles  = toArray(getPath(payload, rolePath)  ?? payload['roles']  ?? payload['role'])
  const groups = toArray(getPath(payload, groupPath) ?? payload['groups'] ?? payload['group'])
  const perms  = toArray(getPath(payload, permPath)  ?? payload['perms']  ?? payload['scope'])

  return { roles, groups, perms }
}

// ── Verification ───────────────────────────────────────────────────────────────

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
  const { roles, groups, perms } = extractClaims(payload, config.preset, config.claimMappings)

  const extra: Record<string, unknown> = {}
  for (const [k, v] of Object.entries(payload)) {
    if (!['sub', 'iss', 'aud', 'exp', 'iat', 'nbf'].includes(k)) extra[k] = v
  }

  return { sub, roles, groups, perms, exp, extra }
}

// ── Env-var config ─────────────────────────────────────────────────────────────

export function jwtConfigFromEnv(): JwtConfig | null {
  const secret    = process.env['SODP_JWT_SECRET']
  const keyFile   = process.env['SODP_JWT_PUBLIC_KEY_FILE']
  const keyInline = process.env['SODP_JWT_PUBLIC_KEY']
  const preset    = (process.env['SODP_JWT_PRESET'] as JwtPreset | undefined)

  if (keyFile) {
    const pem = readFileSync(keyFile, 'utf-8')
    return { type: 'rs256', publicKeyPem: pem, preset }
  }
  if (keyInline) {
    return { type: 'rs256', publicKeyPem: keyInline.replace(/\\n/g, '\n'), preset }
  }
  if (secret) {
    return { type: 'hs256', secret, preset }
  }
  return null
}
