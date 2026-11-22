# Releasing

How a release is cut, checked and published. 0.1.4 and 0.1.5 went out this way. Every
release is a pre-release on GitHub with three files:
- the arm64 APK;
- the Apple-silicon macOS binary;
- `SHA256SUMS`.

It needs an Apple-silicon Mac with:
- Rust;
- the Android SDK with an NDK and the `aarch64-linux-android` target;
- JDK 17;
- an arm64 emulator image (see `scripts/android-smoke.sh`);
- `gh`, logged in to the repository;
- cargo-about 0.9.3 (`cargo install --locked --features cli cargo-about@0.9.3`), for the
  third-party notices.

The release key lives outside the repository. `android/keystore.properties` names it and is
git-ignored.

## 1. Before branching

- **CI:** main is green, and the PRs meant for the release are merged.
- **GeckoView:** check
  `https://maven.mozilla.org/maven2/org/mozilla/geckoview/geckoview-arm64-v8a/maven-metadata.xml`.
  If its `<release>` is newer than the version in `android/app/build.gradle.kts`, update
  GeckoView first: each Firefox release carries security fixes.
- **Changelog:** `CHANGELOG.md` has an `## Unreleased` section that covers the changes.

## 2. The release commit

On a branch `release/<version>` from main:

1. Set `version` in `Cargo.toml`'s `[workspace.package]`, then run
   `cargo update --workspace --offline` to update `Cargo.lock`.
2. In `android/app/build.gradle.kts`, raise `versionCode` by one and set `versionName`.
   Android installs an update only over a lower version code signed with the same key.
3. In `CHANGELOG.md`, rename `## Unreleased` to `## <version> (<YYYY-MM-DD>)`.
4. Commit as `Release <version>`, push, and open a pull request against main.

Merge the release PR right after publishing, before branching anything else. A fix
branched earlier conflicts with it on the changelog's `Unreleased` heading.

## 3. Gates, at the release commit

Run both from a clean checkout of the release commit:

```sh
./.agents/bin/eng verify --profile full --ci   # tests and the security scanners
./.agents/bin/eng verify --profile fast --ci   # fmt and clippy, which the full profile does not run
```

`release-check` (step 7) requires passing full-profile evidence for `HEAD`.

## 4. Build

```sh
cargo build --release --locked -p omnid
mkdir -p target/dist/omnid-<version>-aarch64-apple-darwin
cp target/release/omnid LICENSE README.md CHANGELOG.md target/dist/omnid-<version>-aarch64-apple-darwin/
scripts/third-party-notices.py macos target/dist/omnid-<version>-aarch64-apple-darwin/THIRD-PARTY-NOTICES.txt
(cd target/dist && COPYFILE_DISABLE=1 tar -czf omnid-<version>-aarch64-apple-darwin.tar.gz omnid-<version>-aarch64-apple-darwin)

android/build-rust.sh release
scripts/third-party-notices.py android android/app/src/main/assets/licenses/third-party.txt
(cd android && ./gradlew assembleRelease)
```

The notices cover the Rust crates in each build and, in the app, the Java and Kotlin libraries
that come with GeckoView. The script stops at a license it does not know rather than leave a
notice out. CI's Android job runs it for both builds on every pull request, so such a
license shows up there first. The app's copy is git-ignored, and `assembleRelease` packs it
into the APK.

`target/dist/` keeps the files out of the tree, which `release-check` requires to be clean.
`COPYFILE_DISABLE=1` keeps macOS metadata files out of the tarball.

- **Where `keystore.properties` exists,** `assembleRelease` signs with the release key.
- **On a build machine without it,** the APK comes out signed with the debug key. Copy it
  to the machine that holds the key and re-sign it there:

  ```sh
  KS_PASS=… KEY_PASS=… apksigner sign --ks <storeFile> --ks-key-alias <keyAlias> \
    --ks-pass env:KS_PASS --key-pass env:KEY_PASS \
    --v1-signing-enabled false --v2-signing-enabled true --v3-signing-enabled true \
    --alignment-preserved --out omnid-<version>-arm64.apk app-release.apk
  ```

  Read the passwords from `keystore.properties` into the environment. Never put them on a
  command line, in shell history or in a log.

Then check the APK and write the checksums:

```sh
apksigner verify -v --print-certs omnid-<version>-arm64.apk
aapt2 dump badging omnid-<version>-arm64.apk | head -1
unzip -l omnid-<version>-arm64.apk | grep assets/licenses/third-party.txt
shasum -a 256 omnid-<version>-arm64.apk omnid-<version>-aarch64-apple-darwin.tar.gz > SHA256SUMS
```

- `apksigner` must report the certificate SHA-256 digest
  `7ca1d0f66bfa1e50e3c258b158779ef7aa6957c1ef00c2c18a385659af812a53`.
- `aapt2` must show the new `versionCode` and `versionName`.
- `unzip` must list the notices. An APK without them must not be published.

## 5. Test the artifacts themselves

Test the files that will be published, not a fresh build.

**The binary on a first run.** Extract the tarball into an empty directory. Then run
`omnid --no-tor --no-dht --data-dir <a path that does not exist> serve --listen
127.0.0.1:<port>` and request `/api/status`. It must answer with the new version.
- 0.1.2 to 0.1.4 failed this check, and every other check missed it, because each one ran
  `crawl` first.

**An upgrade on the emulator.**
1. Run `scripts/android-smoke.sh --fresh <previous release's APK>`, and let it crawl for
   about two minutes.
2. With `adb root`, read `files/omnid/crawl.sqlite` in the app's data directory:
   - `SELECT count(*) FROM pages`;
   - `SELECT count(*) FROM pages WHERE visits > 0`.

   Push the SQL in a file. Inline quotes break when the command goes over ssh.
3. Run `adb logcat -c`, then `scripts/android-smoke.sh <new APK>`, which installs it in
   place.
4. The smoke test must pass.
5. A few minutes later, both counts must have grown from where they were.
6. `adb logcat -d` must contain no `panicked at` and no `FATAL EXCEPTION`. The panic hook
   logs a task's panic as an error without crashing the app, so the smoke test cannot see
   one. 0.1.2 found reqwest's platform verifier panicking on every Android HTTPS fetch
   this way.

**Rollback compatibility.**
1. Crawl a new data directory with the new binary for 150 s:
   `omnid --data-dir data crawl --max-depth 1 --max-site-depth 2`. Stop it with SIGINT.
2. With the previous release's binary, run `search tor` on that directory.
3. With the same binary, run `serve` on it and request `/api/status` and `/api/search?q=tor`.

Record the numbers. They go into the notes and the rollback plan.

## 6. Notes and rollback plan

Write two files outside the repository, as earlier releases did:

**`NOTES.md`**, the release's GitHub text:
- what changed, for a user;
- known issues;
- upgrading, with the emulator numbers from step 5;
- the downloads table;
- how to check a download: `shasum -a 256 -c SHA256SUMS`, and `apksigner verify
  --print-certs`, which must report the certificate above;
- the commit the release was built from, and the gate it passed.

**`ROLLBACK.md`:**
- the owner;
- how to withdraw the release: `gh release delete v<version> -R amiriteorg/omnid
  --cleanup-tag --yes`;
- what a device already on it can do: Android refuses a lower version code, so such a
  device waits for the next release;
- whether the previous release reads the new data, with step 5's numbers;
- how much of the plan was tested.

## 7. release-check

Run it once per artifact, at least a second apart: runs in the same second share an evidence
file name.

```sh
./.agents/bin/eng release-check --version <version> --artifact <file> \
  --notes NOTES.md --rollback ROLLBACK.md
```

It checks:
- that the tree is clean;
- the version format;
- that the changelog names the version;
- the notes and the rollback plan;
- the full-profile evidence for `HEAD`;
- each artifact's hash.

## 8. Publish

```sh
gh release create v<version> -R amiriteorg/omnid --prerelease --target <full commit SHA> \
  --title "OMNId <version>" --notes-file NOTES.md \
  omnid-<version>-arm64.apk omnid-<version>-aarch64-apple-darwin.tar.gz SHA256SUMS
```

`--target` takes the release commit's full SHA. `gh` creates the tag there.

## 9. After publishing

Download the release into an empty directory, then:

1. Run `shasum -a 256 -c SHA256SUMS`.
2. Run `apksigner verify --print-certs` on the APK and compare the certificate.
3. Extract the tarball. The binary must report the version and pass the first-run `serve`
   check from step 5.
4. `git ls-remote --tags origin v<version>` must print the release commit.

Then merge the release PR.
