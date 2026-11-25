// Validates orca-plugin.json against Orca's v2 JSON schema (path: ORCA_SCHEMA or the default checkout).
import { readFileSync, existsSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import Ajv from 'ajv/dist/2020.js'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const schemaPath = process.env.ORCA_SCHEMA || '/home/user/stablyai/orca/docs/schemas/plugins/orca-plugin.v2.schema.json'
if (!existsSync(schemaPath)) {
  console.error(`schema not found: ${schemaPath} (set ORCA_SCHEMA)`)
  process.exit(2)
}
const schema = JSON.parse(readFileSync(schemaPath, 'utf8'))
const manifest = JSON.parse(readFileSync(join(root, 'orca-plugin.json'), 'utf8'))
const ajv = new Ajv({ allErrors: true, strict: false })
const validate = ajv.compile(schema)
if (validate(manifest)) {
  console.log('orca-plugin.json: valid')
} else {
  console.error(JSON.stringify(validate.errors, null, 2))
  process.exit(1)
}
