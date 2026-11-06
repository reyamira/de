# Release checklist

Return to the [README](../README.md) for installation and usage, or see the [development guide](development.md) for routine checks.

1. Update `version` in `Cargo.toml`, then run `cargo check` to update `Cargo.lock`.
2. Run `cargo fmt --check`, `cargo clippy --locked --all-targets --all-features -- -D warnings`,
   `cargo test --locked --all-targets --all-features`, `cargo build --locked --release`,
   and `cargo +1.88.0 check --locked --all-targets --all-features`.
3. Run `target/release/de --version` and `target/release/de languages`.
4. Commit the changes, then run `cargo publish --dry-run --locked` from the clean checkout.
5. Push the commit to `main`, then create and push the matching tag,
   for example `git tag v0.4.0 && git push origin v0.4.0`.
6. Check the GitHub Actions release workflow and the resulting archives and checksums.
   The tag triggers GitHub publication automatically after CI and package validation.
7. Publish the crate manually with `cargo publish --locked` using crates.io credentials.
   Confirm the new version on crates.io and the GitHub release after publication.
