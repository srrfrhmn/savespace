# Generated API clients

Phase 1 adds this folder's content. Do not edit generated files by hand.

The `api` crate generates an OpenAPI 3.1 spec from Rust types with `utoipa` (design doc §11.1). CI generates these clients from the spec and fails if they are out of date:

| Folder | Language | Used by |
| --- | --- | --- |
| `ts/` | TypeScript | `apps/web` |
| `swift/` | Swift | `apps/ios` |
| `kotlin/` | Kotlin | `apps/android` |
