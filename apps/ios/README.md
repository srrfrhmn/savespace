# SaveSpace iOS (Phase 2)

This folder is a placeholder. The iOS app starts in Phase 2 (design doc §16). It needs macOS and Xcode.

## Planned stack

Swift 6, SwiftUI, Swift Concurrency, Keychain, XCTest, XCUITest (design doc §8.5).

## Planned structure

```
apps/ios/
├── SaveSpace.xcodeproj
├── SaveSpace/               # App target: SwiftUI views, navigation
├── Packages/
│   ├── SaveSpaceCore/       # Swift package that wraps the UniFFI XCFramework
│   └── SaveSpaceAPI/        # Generated OpenAPI client (from clients/)
├── SaveSpaceTests/          # XCTest, including the core golden-file test
└── SaveSpaceUITests/        # XCUITest smoke set
```

## Shared core

The app uses `savespace-core` through UniFFI (`bindings/uniffi`). Phase 2 adds:

1. A `uniffi-bindgen` tool crate to generate Swift sources.
2. A build script that compiles the Rust library for `aarch64-apple-ios` and `aarch64-apple-ios-sim` and packages an XCFramework.
3. An XCTest that reads `crates/core/tests/golden/format.json` and checks every case. iOS MUST produce the same strings as the native and WASM builds (design doc §8.4).
