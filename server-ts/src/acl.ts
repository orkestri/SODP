// Pattern-based ACL matching the Rust server grammar.
//
// Rule format (acl.json):
//   [{ "pattern": "user.{sub}.*", "read": ["{sub}"], "write": ["{sub}"] }]
//
// Pattern segments (dot-separated):
//   literal  — exact match ("game", "scores")
//   {sub}    — captures the JWT sub claim; one segment only
//   *        — matches one or more remaining segments (only valid at end)
//
// Permission expressions:
//   "*"        — anyone (including unauthenticated)
//   "{sub}"    — claims.sub must equal the captured {sub}
//   "role:X"   — claims.roles includes X
//   "group:X"  — claims.groups includes X
//   "perm:X"   — claims.perms includes X

import { readFileSync } from 'fs'
import type { SodpClaims } from './jwt'

export interface AclRule {
  pattern: string
  read: string[]
  write: string[]
}

interface CompiledRule {
  segments: Array<{ type: 'literal'; value: string } | { type: 'capture' } | { type: 'wildcard' }>
  read: string[]
  write: string[]
}

function compileRule(rule: AclRule): CompiledRule {
  const parts = rule.pattern.split('.')
  const segments: CompiledRule['segments'] = parts.map((p) => {
    if (p === '*') return { type: 'wildcard' }
    if (p === '{sub}') return { type: 'capture' }
    return { type: 'literal', value: p }
  })
  return { segments, read: rule.read, write: rule.write }
}

function matchPattern(
  compiled: CompiledRule,
  keySegments: string[],
): { matched: boolean; captures: { sub?: string } } {
  const { segments } = compiled
  const captures: { sub?: string } = {}

  let ki = 0
  for (let si = 0; si < segments.length; si++) {
    const seg = segments[si]
    if (seg.type === 'wildcard') {
      // Wildcard must be last and must consume at least one key segment.
      if (si !== segments.length - 1) return { matched: false, captures }
      if (ki >= keySegments.length) return { matched: false, captures }
      return { matched: true, captures }
    }
    if (ki >= keySegments.length) return { matched: false, captures }
    if (seg.type === 'literal') {
      if (keySegments[ki] !== seg.value) return { matched: false, captures }
    } else {
      // capture
      captures.sub = keySegments[ki]
    }
    ki++
  }

  return { matched: ki === keySegments.length, captures }
}

function checkPermission(
  perms: string[],
  claims: SodpClaims | null,
  captures: { sub?: string },
): boolean {
  for (const p of perms) {
    if (p === '*') return true
    if (!claims) continue
    if (p === '{sub}' && captures.sub !== undefined && claims.sub === captures.sub) return true
    if (p.startsWith('role:')  && claims.roles.includes(p.slice(5))) return true
    if (p.startsWith('group:') && claims.groups.includes(p.slice(6))) return true
    if (p.startsWith('perm:')  && claims.perms.includes(p.slice(5))) return true
    // Literal sub match
    if (claims && p === claims.sub) return true
  }
  return false
}

export class AclRegistry {
  private readonly rules: CompiledRule[]

  constructor(rules: AclRule[]) {
    this.rules = rules.map(compileRule)
  }

  static fromFile(path: string): AclRegistry {
    const raw = readFileSync(path, 'utf-8')
    return new AclRegistry(JSON.parse(raw) as AclRule[])
  }

  // Returns true if the given claims may read the key.
  // If no rule matches, access is allowed (permissive default — same as Rust server).
  canRead(key: string, claims: SodpClaims | null): boolean {
    return this.check(key, claims, 'read')
  }

  canWrite(key: string, claims: SodpClaims | null): boolean {
    return this.check(key, claims, 'write')
  }

  private check(key: string, claims: SodpClaims | null, mode: 'read' | 'write'): boolean {
    const keySegments = key.split('.')
    for (const rule of this.rules) {
      const { matched, captures } = matchPattern(rule, keySegments)
      if (!matched) continue
      const perms = mode === 'read' ? rule.read : rule.write
      return checkPermission(perms, claims, captures)
    }
    // No rule matched → permissive
    return true
  }
}
