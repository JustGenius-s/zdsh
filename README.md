> [!IMPORTANT]
> Remove this line to confirm you've reviewed this PR before submitting.

# zdsh

zdsh is a fork of [Zed](https://github.com/zed-industries/zed). See [docs/zdsh.md](./docs/zdsh.md) for how it differs from Zed.

### Developing zdsh on macOS

zdsh starts from Zed's source tree. The application crate and binary are currently named `zed`.

Install Xcode with the macOS components, select its developer directory, and install CMake with `brew install cmake`. The repository's `rust-toolchain.toml` selects the Rust version, development components, and cross-compilation targets; install them with `rustup show` from the repository root. Check that `xcrun metal --version` succeeds before building; see the [macOS build guide](./docs/development/macos.md) for Metal toolchain setup.

Run these commands from the repository root:

```sh
cargo build --locked -p zed --bin zed
cargo check --locked -p zed --bin zed
cargo run --locked -p zed --bin zed -- --user-data-dir .local-zdsh .
```

The run command stores development settings, databases, and extensions under the Git-ignored `.local-zdsh/` directory. This directory is separate from the installed Zed application's default data directory. On macOS, logs still use `~/Library/Logs/Zed/`.

The project task menu includes `zdsh: build`, `zdsh: check`, and `zdsh: run (isolated data)`. For linting a changed crate, use `./script/clippy --locked -p <crate>`; for its library tests, use `cargo test --locked -p <crate> --lib`.

### Other platforms

- [Building on Linux](./docs/development/linux.md)
- [Building on Windows](./docs/development/windows.md)

### Licensing

Zed source code is licensed primarily under GPL-3.0-or-later, with Apache-2.0 components where marked.

License information for third party dependencies must be correctly provided for CI to pass.

We use [`cargo-about`](https://github.com/EmbarkStudios/cargo-about) to automatically comply with open source licenses. If CI is failing, check the following:

- Is it showing a `no license specified` error for a crate you've created? If so, add `publish = false` under `[package]` in your crate's Cargo.toml.
- Is the error `failed to satisfy license requirements` for a dependency? If so, first determine what license the project has and whether this system is sufficient to comply with this license's requirements. If you're unsure, ask a lawyer. Once you've verified that this system is acceptable add the license's SPDX identifier to the `accepted` array in `script/licenses/zed-licenses.toml`.
- Is `cargo-about` unable to find the license for a dependency? If so, add a clarification field at the end of `script/licenses/zed-licenses.toml`, as specified in the [cargo-about book](https://embarkstudios.github.io/cargo-about/cli/generate/config.html#crate-configuration).
