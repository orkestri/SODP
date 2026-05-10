// Standard SODP RPC method handlers (state.set, state.patch, state.set_in,
// state.delete, state.presence). Called from session.ts for every CALL frame
// whose method name starts with "state.".
//
// Custom application methods are handled by the onCall hook in SodpServer.

import type { StateStore } from './store'
import type { SodpClaims } from './jwt'
import type { SodpSession, CallResult } from './session'
import type { AclRegistry } from './acl'
import { diffShallow } from './jsonpointer'

export async function handleBuiltinCall(
  method: string,
  args: Record<string, unknown>,
  claims: SodpClaims,
  session: SodpSession,
  store: StateStore,
  acl: AclRegistry | null,
): Promise<CallResult | null> {
  switch (method) {
    case 'state.set':     return handleSet(args, claims, session, store, acl)
    case 'state.patch':   return handlePatch(args, claims, session, store, acl)
    case 'state.set_in':  return handleSetIn(args, claims, session, store, acl)
    case 'state.delete':  return handleDelete(args, claims, session, store, acl)
    case 'state.presence': return handlePresence(args, claims, session, store, acl)
    default:              return null // unknown built-in — let the app's onCall handle it
  }
}

function writeAllowed(key: string, claims: SodpClaims, acl: AclRegistry | null): boolean {
  if (!acl) return true
  return acl.canWrite(key, claims)
}

async function handleSet(
  args: Record<string, unknown>,
  claims: SodpClaims,
  _session: SodpSession,
  store: StateStore,
  acl: AclRegistry | null,
): Promise<CallResult> {
  const key = args['state']
  if (typeof key !== 'string') return err('missing state')
  if (!writeAllowed(key, claims, acl)) return err('access denied', 403)

  const snap = store.snapshot(key)
  const ops = diffShallow(snap.value, args['value'])
  if (ops.length > 0) {
    store.publish(key, ops)
  } else if (!snap.initialized) {
    store.hydrate(key, args['value'])
  }
  return { success: true, data: null }
}

async function handlePatch(
  args: Record<string, unknown>,
  claims: SodpClaims,
  _session: SodpSession,
  store: StateStore,
  acl: AclRegistry | null,
): Promise<CallResult> {
  const key = args['state']
  if (typeof key !== 'string') return err('missing state')
  if (!writeAllowed(key, claims, acl)) return err('access denied', 403)

  const patch = args['patch']
  if (!patch || typeof patch !== 'object' || Array.isArray(patch)) {
    return err('patch must be an object')
  }
  const ops = Object.entries(patch as Record<string, unknown>).map(([k, v]) => ({
    op: 'UPDATE' as const,
    path: `/${k.replace(/~/g, '~0').replace(/\//g, '~1')}`,
    value: v,
  }))
  if (ops.length > 0) store.publish(key, ops)
  return { success: true, data: null }
}

async function handleSetIn(
  args: Record<string, unknown>,
  claims: SodpClaims,
  _session: SodpSession,
  store: StateStore,
  acl: AclRegistry | null,
): Promise<CallResult> {
  const key = args['state']
  if (typeof key !== 'string') return err('missing state')
  if (!writeAllowed(key, claims, acl)) return err('access denied', 403)

  const path = args['path']
  if (typeof path !== 'string') return err('missing path')

  store.publish(key, [{ op: 'UPDATE', path, value: args['value'] }])
  return { success: true, data: null }
}

async function handleDelete(
  args: Record<string, unknown>,
  claims: SodpClaims,
  _session: SodpSession,
  store: StateStore,
  acl: AclRegistry | null,
): Promise<CallResult> {
  const key = args['state']
  if (typeof key !== 'string') return err('missing state')
  if (!writeAllowed(key, claims, acl)) return err('access denied', 403)

  store.remove(key)
  return { success: true, data: null }
}

async function handlePresence(
  args: Record<string, unknown>,
  claims: SodpClaims,
  session: SodpSession,
  store: StateStore,
  acl: AclRegistry | null,
): Promise<CallResult> {
  const key = args['state']
  if (typeof key !== 'string') return err('missing state')
  if (!writeAllowed(key, claims, acl)) return err('access denied', 403)

  const path = args['path']
  if (typeof path !== 'string') return err('missing path')

  store.publish(key, [{ op: 'UPDATE', path, value: args['value'] }])
  session.registerPresence(key, path)
  return { success: true, data: null }
}

function err(message: string, _code = 400): CallResult {
  return { success: false, data: message }
}
