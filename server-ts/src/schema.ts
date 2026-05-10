// Schema validation for SODP state keys. Mirrors the Rust server SDL.
//
// Schema file format — a JSON object mapping state keys to SchemaNode values:
//   { "game.player": { "type": "Object", "fields": { "health": "Int" } } }
//
// Unknown keys pass validation (permissive). Invalid writes receive ERROR 422.

import { readFileSync } from 'fs'

// A SchemaNode is either a bare type string ("Int") or a full descriptor.
export type SchemaNode =
  | string  // compact form: just the type name, non-nullable
  | { type: string; nullable?: boolean; fields?: Record<string, SchemaNode> }

function typeName(node: SchemaNode): string {
  return typeof node === 'string' ? node : node.type
}

function isNullable(node: SchemaNode): boolean {
  return typeof node === 'string' ? false : (node.nullable ?? false)
}

function fields(node: SchemaNode): Record<string, SchemaNode> | undefined {
  return typeof node === 'string' ? undefined : node.fields
}

function kindOf(v: unknown): string {
  if (v === null) return 'null'
  if (typeof v === 'boolean') return 'Bool'
  if (typeof v === 'string') return 'String'
  if (typeof v === 'number') return Number.isInteger(v) ? 'Int' : 'Float'
  if (Array.isArray(v)) return 'Array'
  if (typeof v === 'object') return 'Object'
  return typeof v
}

function validateNode(value: unknown, node: SchemaNode, path: string): string | null {
  if (value === null || value === undefined) {
    return isNullable(node) ? null : `${path}: unexpected null (field is not nullable)`
  }

  const t = typeName(node)

  switch (t) {
    case 'Any': return null

    case 'String':
      return typeof value === 'string' ? null : `${path}: expected String, got ${kindOf(value)}`

    case 'Int':
      return (typeof value === 'number' && Number.isInteger(value))
        ? null
        : `${path}: expected Int, got ${kindOf(value)}`

    case 'Float':
      // Widening: integers are valid Float values.
      return typeof value === 'number' ? null : `${path}: expected Float, got ${kindOf(value)}`

    case 'Bool':
      return typeof value === 'boolean' ? null : `${path}: expected Bool, got ${kindOf(value)}`

    case 'Array':
      return Array.isArray(value) ? null : `${path}: expected Array, got ${kindOf(value)}`

    case 'Object': {
      if (typeof value !== 'object' || Array.isArray(value)) {
        return `${path}: expected Object, got ${kindOf(value)}`
      }
      const obj = value as Record<string, unknown>
      const nodeFields = fields(node)
      if (nodeFields) {
        for (const [fieldName, fieldSchema] of Object.entries(nodeFields)) {
          const err = validateNode(obj[fieldName] ?? null, fieldSchema, `${path}.${fieldName}`)
          if (err) return err
        }
      }
      return null
    }

    default:
      return `${path}: unknown schema type '${t}'`
  }
}

export class SchemaRegistry {
  private readonly states: Map<string, SchemaNode>

  constructor(schema: Record<string, SchemaNode>) {
    this.states = new Map(Object.entries(schema))
  }

  static fromFile(path: string): SchemaRegistry {
    const raw = readFileSync(path, 'utf-8')
    const schema = JSON.parse(raw) as Record<string, SchemaNode>
    return new SchemaRegistry(schema)
  }

  // Returns an error message on failure, null on success.
  // Unknown keys always pass (permissive, backwards-compatible).
  validate(key: string, value: unknown): string | null {
    const node = this.states.get(key)
    if (!node) return null
    return validateNode(value, node, key)
  }
}
