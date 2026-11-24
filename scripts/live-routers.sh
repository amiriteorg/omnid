#!/usr/bin/env bash
# Runs omnid's live tests against real routers: Tor (arti, in-process), the
# BitTorrent DHT, a client-only i2pd, a temporary Hyphanet node, a temporary
# IPFS node (kubo) and a temporary Yggdrasil node (yggstack).
#
#   scripts/live-routers.sh up      start i2pd, Hyphanet, kubo and yggstack, wait until they serve
#   scripts/live-routers.sh test    cargo test --workspace -- --ignored (all live tests)
#   scripts/live-routers.sh down    stop them and delete their state
#   scripts/live-routers.sh all     up, test, down
#
# i2pd runs with --notransit, so it relays nobody else's traffic. The Hyphanet
# node joins opennet with a 256 MiB store of other users' encrypted blocks; `down`
# deletes it. kubo is a DHT client only, providing nothing to others, with a
# repo `down` deletes. yggstack peers out to a few public Yggdrasil nodes and
# listens for none; while it runs it is a node like any, which may carry other
# nodes' traffic between its peers. `down` stops it and deletes its keys.
# Downloads are cached in target/live/cache and checked against pinned hashes
# and the Hyphanet release key before anything runs.
#
# Needs: i2pd and kubo (brew install i2pd kubo, or your package manager), Java
# 17+, curl, gpg, python3. Ports: 4444/4447/7070/7656 (i2pd), 8888/9481
# (Hyphanet), 8080/5001 (kubo) and 1080 (yggstack's SOCKS proxy), all on
# 127.0.0.1, and 4001 (kubo's peers).
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

YGGSTACK_VERSION=1.0.7
YGGSTACK_URL=https://github.com/yggdrasil-network/yggstack/releases/download/$YGGSTACK_VERSION
# Public peers from github.com/yggdrasil-network/public-peers (Germany, the
# Netherlands, Sweden). The list pins some peers' public keys (`?key=`); those
# are left out here, as a scanner takes the hex for a secret, and a temporary
# node needs no pin: Yggdrasil encrypts end to end whoever carries the traffic.
YGGSTACK_PEERS='tls://ygg.mkg20001.io:443
tls://cirno.nadeko.net:44442
tls://5.252.118.13:443
tls://dmwp.suckmost.org:8647'

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
      --sam.enabled=true --sam.address=127.0.0.1 --sam.port=7656 \
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

start_kubo() {
  command -v ipfs >/dev/null || die "kubo is not installed (brew install kubo)"
  local repo=$LIVE/kubo
  if [ -f "$repo/kubo.pid" ] && kill -0 "$(cat "$repo/kubo.pid")" 2>/dev/null; then
    say "kubo already running"
  elif nc -z 127.0.0.1 8080 2>/dev/null; then
    # A node of the machine's own, or something else on the port, which the
    # live test then says plainly; `down` leaves it alone.
    say "using what is already on 127.0.0.1:8080"
    return
  else
    say "starting kubo (DHT client only)"
    rm -rf "$repo"
    IPFS_PATH=$repo ipfs init --profile lowpower >/dev/null
    IPFS_PATH=$repo ipfs config Addresses.Gateway /ip4/127.0.0.1/tcp/8080
    IPFS_PATH=$repo ipfs config Addresses.API /ip4/127.0.0.1/tcp/5001
    IPFS_PATH=$repo ipfs config Routing.Type autoclient
    IPFS_PATH=$repo ipfs config --json Provide.Enabled false
    (IPFS_PATH=$repo exec nohup ipfs daemon >"$repo/stdout.log" 2>&1) &
    echo $! >"$repo/kubo.pid"
  fi
  # The identity CID needs no peers: this is the gateway itself answering.
  wait_for 120 "kubo's gateway" curl -fs --max-time 5 -o /dev/null http://127.0.0.1:8080/ipfs/bafkqaaa
  say "kubo ready"
}

start_yggstack() {
  local dir=$LIVE/yggstack asset sha
  case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) asset=yggstack-darwin-arm64 sha=7d6a41ff251fc5544d32fa9e755ae86539f92cb9649b19aa5d30dbe682a9819d ;;
    Darwin-x86_64) asset=yggstack-darwin-amd64 sha=1a26e9494a998e2ab0cd018dd4a7d5a74c9a000dfd0791fab7984c6f61674bfb ;;
    Linux-x86_64) asset=yggstack-linux-amd64-static sha=148bcd928b70ad383e08081f341d7cb1e54d4c5dc444804b79d794a30d7c6555 ;;
    Linux-aarch64) asset=yggstack-linux-arm64-static sha=75513a28938a10cf77afa95f1f86d9f460cc9d3238ca0e37bbae142322f4b448 ;;
    *) die "no yggstack build pinned for $(uname -s)-$(uname -m)" ;;
  esac
  if [ -f "$dir/yggstack.pid" ] && kill -0 "$(cat "$dir/yggstack.pid")" 2>/dev/null; then
    say "yggstack already running"
  elif nc -z 127.0.0.1 1080 2>/dev/null; then
    # A Yggdrasil proxy of the machine's own, or something else on the port,
    # which the live test then says plainly; `down` leaves it alone.
    say "using what is already on 127.0.0.1:1080"
    return
  else
    say "starting yggstack (peers out, listens for none)"
    mkdir -p "$CACHE"
    fetch_pinned "$YGGSTACK_URL/$asset" "$CACHE/$asset-$YGGSTACK_VERSION" "$sha"
    rm -rf "$dir" && mkdir -p "$dir"
    cp "$CACHE/$asset-$YGGSTACK_VERSION" "$dir/yggstack" && chmod +x "$dir/yggstack"
    "$dir/yggstack" -genconf -json >"$dir/conf.json"
    YGGSTACK_PEERS=$YGGSTACK_PEERS python3 - "$dir/conf.json" <<'PY'
import json, os, sys
path = sys.argv[1]
conf = json.load(open(path))
conf.update(Peers=os.environ["YGGSTACK_PEERS"].split(), Listen=[], MulticastInterfaces=[], AdminListen="none")
json.dump(conf, open(path, "w"))
PY
    chmod 600 "$dir/conf.json"
    (exec nohup "$dir/yggstack" -useconffile "$dir/conf.json" -socks 127.0.0.1:1080 >"$dir/yggstack.log" 2>&1) &
    echo $! >"$dir/yggstack.pid"
  fi
  wait_for 60 "yggstack's SOCKS proxy" nc -z 127.0.0.1 1080
  wait_for 90 "a Yggdrasil peer" grep -q "Connected outbound" "$dir/yggstack.log"
  say "yggstack ready"
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
  up) start_i2pd; start_hyphanet; start_kubo; start_yggstack ;;
  # --no-fail-fast: one failing network must not hide the others' results.
  test) cd "$ROOT" && cargo test --workspace --no-fail-fast -- --ignored --test-threads=4 ;;
  down)
    stop "$LIVE/i2pd" i2pd.pid i2pd
    stop "$LIVE/hyphanet" java.pid Hyphanet
    stop "$LIVE/kubo" kubo.pid kubo
    stop "$LIVE/yggstack" yggstack.pid yggstack
    ;;
  all)
    trap '"$0" down' EXIT
    "$0" up
    "$0" test
    ;;
  *) sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
