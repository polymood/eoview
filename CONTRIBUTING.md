# Contributing to eoview

Thanks for your interest in eoview. Bug reports, ideas and pull requests are all welcome.

## Reporting a bug

Open an issue with the **Bug report** form. It helps a lot if you include:

- the version of eoview (Preferences, then About),
- your operating system, and your graphics card if you know it,
- the kind of product you opened (for example a Sentinel-2 SAFE, a NetCDF file from a model, a COG),
- a link to a public sample of the product if there is one, or a small file that shows the problem,
- what you expected, and what happened instead. A screenshot is often the fastest way to explain.

## Suggesting a feature

Open an issue with the **Feature request** form. Tell us what you are trying to do and why. A short
description of your data and of your work helps more than a finished design. `NEXT.md` lists the work
that is already planned.

## Building and testing

You need a recent stable Rust toolchain.

```
cargo build --release
cargo test --release
```

On x86_64 the build targets processors with AVX2 (see `.cargo/config.toml`). The README has more about
the test data, the tests with real products, and the benchmarks.

## Pull requests

- Work happens on the `develop` branch, and `main` is only for releases. Open your pull request against
  `develop`.
- Keep a pull request to one change. Small pull requests are easier to review.
- Before you push, run `cargo clippy --release --all-targets` (it must have no errors) and
  `cargo test --release`. CI runs both on Windows, Linux and macOS.
- Match the layout of the code around your change. The code does not use `cargo fmt`: please do not
  reformat files that you do not change.
- Write comments in the same plain, simple style as the code around them: short full sentences that say
  what the code does and why.
- If you change something that users see, update `README.md`, and the French text in
  `crates/eoview/assets/lang/fr.json` for new interface text.

## Releases

The maintainer makes the releases: the version goes up in `Cargo.toml`, `develop` is merged into `main`,
and a tag `vX.Y.Z` starts the release workflow. The README section "Release" has the details.

## Code of conduct

Everyone who takes part in this project agrees to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
