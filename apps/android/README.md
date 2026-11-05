# SaveSpace Android (Phase 3)

This folder is a placeholder. The Android app starts in Phase 3 (design doc §16).

## Planned stack

Kotlin, Jetpack Compose, Coroutines, Android Keystore (design doc §8.5).

## Planned structure

```
apps/android/
├── app/                     # Compose UI, navigation
├── core/                    # Kotlin module that wraps the UniFFI AAR
├── api/                     # Generated OpenAPI client (from clients/)
└── build.gradle.kts
```

## Shared core

The app uses `savespace-core` through UniFFI (`bindings/uniffi`). Phase 3 adds a build that compiles the Rust library for the Android ABIs (with `cargo-ndk`) and packages an AAR. A unit test reads `crates/core/tests/golden/format.json` and checks every case (design doc §8.4).
