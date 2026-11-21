# Security

OMNId's job is to keep its user anonymous on Tor, I2P and Hyphanet and safe
from what it crawls. A flaw in either is a security issue. These come first:

- **Deanonymization:** any way a request leaves outside Tor, I2P or the
  Hyphanet node when it should not, a DNS lookup, or content on one network
  making OMNId connect on another from the user's own address.
- **Linkability:** browser sites, the crawler, downloads, or apps behind the
  proxy sharing circuits they should not, and fingerprinting the browser
  beyond what resist-fingerprinting allows.
- **The local attack surface:** another app on the phone using the API, the
  browser's proxy or the proxy for other apps without the secret, or getting
  at the app's data.
- **Hostile input:** crashes, hangs or unbounded memory from what a site,
  router or peer sends.

The README's [Anonymity](README.md#anonymity) section says what OMNId hides
and what it does not; its [Known limitations](README.md#known-limitations)
are not vulnerabilities, but a way around one of them is.

## Reporting

Report privately through GitHub: the repository's **Security** tab →
**Report a vulnerability**. Please don't open a public issue or pull request
for it. Say what is affected (app or daemon, version), how to reproduce it,
and what an attacker gains. Expect an answer within a week; a fix goes into
the next release, credited to you unless you would rather not be named.

## Supported versions

Only the latest release gets fixes. OMNId is pre-release software; update to
the newest version before reporting.

## Verifying releases

Each release lists SHA-256 checksums (`SHA256SUMS`). The Android app is
signed with one key, certificate SHA-256
`7ca1d0f66bfa1e50e3c258b158779ef7aa6957c1ef00c2c18a385659af812a53`; an APK
with any other signature is not OMNId's.
