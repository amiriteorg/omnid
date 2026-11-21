#!/usr/bin/env bash
# Runs omnid's live tests against real routers: Tor (arti, in-process), the
# BitTorrent DHT, a client-only i2pd and a temporary Hyphanet node.
#
#   scripts/live-routers.sh up      start i2pd and Hyphanet, wait until they serve
#   scripts/live-routers.sh test    cargo test --workspace -- --ignored (all live tests)
#   scripts/live-routers.sh down    stop both and delete their state
#   scripts/live-routers.sh all     up, test, down
#
# i2pd runs with --notransit, so it relays nobody else's traffic. The Hyphanet
# node joins opennet with a 256 MiB store of other users' encrypted blocks; `down`
# deletes it. Downloads are cached in target/live/cache and checked against
# pinned hashes and the Hyphanet release key before anything runs.
#
# Needs: i2pd (brew install i2pd, or your package manager), Java 17+, curl,
# gpg. Ports: 4444/4447/7070 (i2pd) and 8888/9481 (Hyphanet), all on 127.0.0.1.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
LIVE=$ROOT/target/live
CACHE=$LIVE/cache

HYPHANET_BUILD=1507
HYPHANET_DEB=freenet_0.7.5+${HYPHANET_BUILD}-1_amd64.deb
HYPHANET_URL=https://github.com/hyphanet/fred/releases/download/build0${HYPHANET_BUILD}
HYPHANET_DEB_SHA256=0dda10df155be6eac88a6124c060e9a21ab9a85b346d034e382a6f65c75bb4d4
# Fingerprint of the release signing key, Arne Babenhauserheide (freenet
# releases), as published on hyphanet.org. Public, not a secret.
HYPHANET_SIGNER_FINGERPRINT=B30C3D91069F81ECFEFED0B1B41A6047FD6C57F9
# The bundled JNA 4.5.2 has no arm64 macOS library; 5.x is API compatible here.
JNA_VERSION=5.19.1
JNA_SHA256=4fb141dd8ef6b0585ffceea4bc49602fbc6312fa977e2c488794ea3e6aafecae
JNA_PLATFORM_SHA256=3b3864f5b449e9c3c24b16861524b622b086563f44e0cd8384c8efc5a6052f82
# A freesite every node bookmarks, to tell when Hyphanet can serve content.
HYPHANET_PROBE='USK@62H8KFSZWMyQ2MQgwvNhEYJ2m3SQl696PfsVfWQ-HQo,cJrPvdNz4AnrHJQXNteDV7k3YnAVY-MClt84gwH2qEo,AQACAAE/freenet-first-steps/24/'

say() { printf '%s\n' "live: $*" >&2; }
die() { say "$*"; exit 1; }

sha256() { shasum -a 256 "$1" 2>/dev/null | cut -d' ' -f1 || sha256sum "$1" | cut -d' ' -f1; }

# fetch URL FILE SHA256
fetch_pinned() {
  [ -f "$2" ] && [ "$(sha256 "$2")" = "$3" ] && return 0
  curl -fsSL -o "$2.part" "$1" || die "download failed: $1"
  [ "$(sha256 "$2.part")" = "$3" ] || { rm -f "$2.part"; die "checksum mismatch: $1"; }
  mv "$2.part" "$2"
}

wait_for() { # SECONDS DESCRIPTION COMMAND...
  local deadline=$((SECONDS + $1)) what=$2
  shift 2
  until "$@" >/dev/null 2>&1; do
    [ $SECONDS -lt $deadline ] || die "timed out waiting for $what"
    sleep 3
  done
}

start_i2pd() {
  command -v i2pd >/dev/null || die "i2pd is not installed (brew install i2pd)"
  local dir=$LIVE/i2pd
  if [ -f "$dir/i2pd.pid" ] && kill -0 "$(cat "$dir/i2pd.pid")" 2>/dev/null; then
    say "i2pd already running"
  elif nc -z 127.0.0.1 4444 2>/dev/null; then
    # A router of the machine's own (one kept running for omnid): use it as
    # it is; `down` leaves it alone, having no PID file for it.
    say "using the I2P router already on 127.0.0.1:4444"
    return
  else
    mkdir -p "$dir"
    say "starting i2pd (client only)"
    # exec, so no shell stays behind waiting on i2pd and holding our stdout.
    (cd "$dir" && exec nohup i2pd --datadir="$dir" --notransit --upnp.enabled=false \
      --httpproxy.address=127.0.0.1 --httpproxy.port=4444 \
      --socksproxy.address=127.0.0.1 --socksproxy.port=4447 \
      --http.address=127.0.0.1 --http.port=7070 \
      --log=file --logfile="$dir/i2pd.log" --loglevel=warn --pidfile="$dir/i2pd.pid" >"$dir/stdout.log" 2>&1) &
  fi
  # Tunnels come up first; names resolve once the address book has loaded.
  wait_for 120 "i2pd's proxy" nc -z 127.0.0.1 4444
  wait_for 600 "i2pd's address book" grep -qE '^stats\.i2p,' "$dir/addressbook/addresses.csv"
  say "i2pd ready"
}

install_hyphanet() {
  local pkg=$CACHE/hyphanet-$HYPHANET_BUILD
  [ -f "$pkg/usr/share/java/freenet.jar" ] && return 0
  command -v gpg >/dev/null || die "gpg is needed to verify Hyphanet"
  mkdir -p "$CACHE"
  fetch_pinned "$HYPHANET_URL/$HYPHANET_DEB" "$CACHE/$HYPHANET_DEB" "$HYPHANET_DEB_SHA256"
  curl -fsSL -o "$CACHE/$HYPHANET_DEB.sig" "$HYPHANET_URL/$HYPHANET_DEB.sig"
  # A throwaway keyring: the key is imported, its fingerprint checked, and
  # the signature must be valid for exactly that key.
  local gnupg
  gnupg=$(mktemp -d "$CACHE/gpg.XXXXXX")
  curl -fsSL "https://keyserver.ubuntu.com/pks/lookup?op=get&options=mr&search=0x$HYPHANET_SIGNER_FINGERPRINT" \
    | GNUPGHOME=$gnupg gpg --batch --quiet --import 2>/dev/null
  GNUPGHOME=$gnupg gpg --batch --status-fd 1 --verify "$CACHE/$HYPHANET_DEB.sig" "$CACHE/$HYPHANET_DEB" 2>/dev/null \
    | grep -q "VALIDSIG $HYPHANET_SIGNER_FINGERPRINT" || { rm -rf "$gnupg"; die "Hyphanet signature did not verify"; }
  rm -rf "$gnupg"
  mkdir -p "$pkg/extract"
  (cd "$pkg/extract" && tar -xf "$CACHE/$HYPHANET_DEB" && tar -xf data.tar.* -C "$pkg")
  rm -rf "$pkg/extract"
  fetch_pinned "https://repo1.maven.org/maven2/net/java/dev/jna/jna/$JNA_VERSION/jna-$JNA_VERSION.jar" \
    "$pkg/jna-$JNA_VERSION.jar" "$JNA_SHA256"
  fetch_pinned "https://repo1.maven.org/maven2/net/java/dev/jna/jna-platform/$JNA_VERSION/jna-platform-$JNA_VERSION.jar" \
    "$pkg/jna-platform-$JNA_VERSION.jar" "$JNA_PLATFORM_SHA256"
  say "Hyphanet $HYPHANET_BUILD verified and unpacked"
}

start_hyphanet() {
  command -v java >/dev/null || die "Java 17+ is needed for Hyphanet"
  install_hyphanet
  local pkg=$CACHE/hyphanet-$HYPHANET_BUILD node=$LIVE/hyphanet
  if [ -f "$node/java.pid" ] && kill -0 "$(cat "$node/java.pid")" 2>/dev/null; then
    say "Hyphanet already running"
  elif nc -z 127.0.0.1 8888 2>/dev/null; then
    # A node of the machine's own (one kept running for omnid): use it as it
    # is; `down` leaves it alone, having no PID file for it.
    say "using the Hyphanet node already on 127.0.0.1:8888"
  else
    mkdir -p "$node"/{state,run,store,plugins,tmp,downloads,logs}
    cp "$pkg/usr/share/freenet/seednodes.fref" "$node/"
    cat >"$node/freenet.ini" <<EOF
node.install.nodeDir=$node
node.install.cfgDir=$node
node.install.userDir=$node/state
node.install.runDir=$node/run
node.install.storeDir=$node/store
node.install.pluginDir=$node/plugins
node.install.tempDir=$node/tmp
node.downloadsDir=$node/downloads
logger.dirname=$node/logs
logger.priority=ERROR
node.updater.enabled=false
node.updater.autoupdate=false
node.opennet.enabled=true
node.storeSize=256MiB
node.clientCacheSize=16MiB
node.slashdotCacheSize=16MiB
node.outputBandwidthLimit=64KiB
node.inputBandwidthLimit=-1
security-levels.networkThreatLevel=NORMAL
security-levels.physicalThreatLevel=LOW
fproxy.enabled=true
fproxy.port=8888
fproxy.bindTo=127.0.0.1
fproxy.allowedHosts=127.0.0.1,localhost
fproxy.hasCompletedWizard=true
fcp.enabled=true
fcp.bindTo=127.0.0.1
fcp.allowedHosts=127.0.0.1
End
EOF
    local java=$pkg/usr/share/java
    local cp=$java/freenet.jar:$java/freenet-ext-29.jar:$pkg/jna-$JNA_VERSION.jar:$pkg/jna-platform-$JNA_VERSION.jar
    cp=$cp:$java/bcprov-jdk15on-1.59.jar:$java/pebble-3.1.5.jar:$java/unbescape-1.1.6.RELEASE.jar:$java/slf4j-api-1.7.25.jar
    say "starting Hyphanet $HYPHANET_BUILD"
    # exec, so the background process is Java itself, its PID is the one
    # recorded, and no shell stays behind holding our stdout.
    (cd "$node" && exec nohup java -cp "$cp" -Dnetworkaddress.cache.ttl=0 -Djava.io.tmpdir="$node/tmp" \
      --add-opens=java.base/java.lang=ALL-UNNAMED --add-opens=java.base/java.util=ALL-UNNAMED \
      --add-opens=java.base/java.io=ALL-UNNAMED -Xss512k -Xmx512m \
      freenet.node.NodeStarter "$node/freenet.ini" >"$node/stdout.log" 2>&1) &
    echo $! >"$node/java.pid"
  fi
  wait_for 180 "FProxy" nc -z 127.0.0.1 8888
  wait_for 900 "Hyphanet to serve a freesite" \
    curl -fs --max-time 120 -o /dev/null "http://127.0.0.1:8888/$HYPHANET_PROBE"
  say "Hyphanet ready"
}

stop() { # DIR PIDFILE NAME
  local pid
  if [ -f "$1/$2" ] && pid=$(cat "$1/$2") && kill "$pid" 2>/dev/null; then
    say "stopping $3"
    for _ in $(seq 1 30); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
    if kill -0 "$pid" 2>/dev/null; then
      say "$3 ignored SIGTERM for 30 s; killing it"
      kill -9 "$pid" 2>/dev/null || true
    fi
  fi
  rm -rf "$1"
}

case "${1:-}" in
  up) start_i2pd; start_hyphanet ;;
  # --no-fail-fast: one failing network must not hide the others' results.
  test) cd "$ROOT" && cargo test --workspace --no-fail-fast -- --ignored --test-threads=4 ;;
  down) stop "$LIVE/i2pd" i2pd.pid i2pd; stop "$LIVE/hyphanet" java.pid Hyphanet ;;
  all)
    trap '"$0" down' EXIT
    "$0" up
    "$0" test
    ;;
  *) sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
