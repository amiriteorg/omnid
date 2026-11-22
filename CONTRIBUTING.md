# Contributing

Thanks for helping. Read these first:
- [README.md](README.md): what OMNId does and how to run it;
- [AGENTS.md](AGENTS.md): the repository map, the commands and the rules the code keeps;
- [ROADMAP.md](ROADMAP.md): what is planned.

**Security problems** go privately through [SECURITY.md](SECURITY.md), never into a public
issue or pull request.

## Before a pull request

Run the checks that CI runs:

```sh
./.agents/bin/eng verify --profile fast       # fmt, clippy -D warnings, tests
./.agents/bin/eng security                    # gitleaks, semgrep, osv-scanner, actionlint
(cd android && ./gradlew testDebugUnitTest)   # when the change touches android/
```

`eng verify` needs only Rust. `eng security` uses whichever scanners are installed and says
which it skipped. The CLI works without the kit too: `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.

CI also runs:
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked`.

**A bug fix comes with a test that fails without it.** Live tests against real networks are
`#[ignore]`d. They run with `scripts/live-routers.sh all` and are never needed for CI.

## Fuzzing

The parsers that read what comes off the network have cargo-fuzz targets in `fuzz/`, a crate
outside the workspace, so the checks above never build it. It needs a nightly toolchain and
cargo-fuzz, at the versions CI uses:

```sh
rustup toolchain install nightly-2026-11-01 --profile minimal
cargo install --locked cargo-fuzz@0.13.2
cd fuzz
cp ../Cargo.lock .   # the versions that ship; cargo adds only the fuzzing crates
cargo +nightly-2026-11-01 fuzz list
cargo +nightly-2026-11-01 fuzz run source_parse corpus/source_parse seeds/source_parse -- \
  -dict=dicts/source_parse.dict -max_total_time=600
```

On macOS, first `export RUSTFLAGS=-Cllvm-args=-asan-globals=0`. Otherwise the targets that
link arti fail with `ld: initializer pointer has no target`: AddressSanitizer pads the
constructors `inventory` puts in `__mod_init_func`. Linux needs no such flag.

- `seeds/<target>` holds a few inputs taken from the tests. `corpus/` (git-ignored) keeps what
  the fuzzer finds, and the first directory named is where it goes.
- `dicts/<target>.dict`, where there is one, lists the format's tokens for the fuzzer to splice
  in. Without its dictionary, `source_parse` ran 9.6 million inputs in 11 minutes without
  finding what a space before a `#` did to a Hyphanet key; with it, it took half a minute.
- A crash is written to `artifacts/<target>/`, and `cargo fuzz run <target> <file>` replays
  it. `cargo fuzz tmin <target> <file>` shrinks it. The fix comes with a unit test that holds
  the shrunk input, in the crate the parser lives in.
- A new parser of network input gets a target. Private parsers are reached through a
  `#[cfg(fuzzing)] mod fuzz` beside their tests, re-exported from the crate's `fuzzing`
  module, which exists only when cargo-fuzz builds it.

## What the code keeps

These hold everywhere. `AGENTS.md` has the full list.

- **Network connections:** only `omnid-net` and `omnid-dht` open them.
- **The app's browser** reaches the network only through the daemon's browser proxy.
- **Hostile input:** everything from the network is hostile. A parser comes with a bound,
  and a test that reaches it. Nothing may panic on malformed input.
- **Downloads:** the crawler never downloads media, and nothing downloads a file the user
  did not ask for.
- **Tests stay off the network.**
- **Contracts:** the HTTP API, the CLI flags, the index schema (`index-v3`), `crawl.sqlite`
  and the JNI symbols are contracts. Change them on purpose, say so, and update `README.md`.

## Shape of a change

- **One idea per commit.** Keep formatting out of behavior changes.
- **Tell users what changed.** A change they can see adds a line under `## Unreleased` in
  `CHANGELOG.md`.
- **No new dependency** without a reason you can show. Every dependency ends up in the
  APK, and in what an attacker can reach.
- **Android:** the app uses platform APIs only, plus GeckoView for the browser.

## License

OMNId is MIT licensed ([LICENSE](LICENSE)). By contributing you agree that your
contribution is under the same license.
