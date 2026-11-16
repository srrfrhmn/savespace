# Generated API clients

Do not edit generated files by hand. Run `pnpm gen:api` after you change an API type or route.

- `openapi.json`: the OpenAPI 3.1 document. The `api` crate writes it, and a Rust test fails if it is out of date.
- `ts/schema.d.ts`: TypeScript types for `openapi-fetch`. CI regenerates them and fails on a difference.

The `api` crate generates an OpenAPI 3.1 spec from Rust types with `utoipa` (design doc §11.1). CI generates these clients from the spec and fails if they are out of date:

| Folder | Language | Used by |
| --- | --- | --- |
| `ts/` | TypeScript | `apps/web` |
| `swift/` | Swift | `apps/ios` |
| `kotlin/` | Kotlin | `apps/android` |
