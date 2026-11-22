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

**A bug fix comes with a test that fails without it.** Live tests against real networks are
`#[ignore]`d. They run with `scripts/live-routers.sh all` and are never needed for CI.

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
