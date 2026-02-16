# macOS Containerization FFI Research (2026-02-14)

## Scope
Assess feasibility of replacing the current `sandbox-exec` interim backend with Apple Containerization APIs from Rust.

## Primary Findings
- Apple publishes a `containerization` Swift package with modules like `Containerization`, `ContainerizationOCI`, and `ContainerizationLinuxKernel`.
- The package requires modern Apple toolchains/OS versions (Xcode 26+, macOS Tahoe 26+).
- The API surface is Swift-native. There is no stable C ABI to call directly from Rust.

## Rust Integration Implication
- Direct Rust FFI into Containerization is not practical without an intermediate layer.
- Recommended implementation path is a small Swift helper binary/library that exposes a narrow, stable CLI/IPC contract consumed by Rust.
- Keep `sandbox-exec` as the active backend until the Swift shim exists and parity tests pass.

## Isolation Parity Target
Parity criteria versus Linux namespace runner:
- Writes allowed in declared writable scopes.
- Writes blocked outside declared scope.
- Nested scope narrowing remains enforced by `tmax-sandbox` before process launch.

## Sources
- Apple Containerization docs: <https://developer.apple.com/documentation/containerization>
- Apple containerization package repository: <https://github.com/apple/containerization>
