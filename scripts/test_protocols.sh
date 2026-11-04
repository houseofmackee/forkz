#!/bin/sh
# Runs the live FTP/FTPS, WebDAV, and S3 tests against throwaway servers on
# 127.0.0.1: pyftpdlib (FTP with explicit TLS, a self-signed certificate),
# WsgiDAV, and moto (an S3 emulator). They run from a Python virtual
# environment in a temporary folder that is removed afterwards, along with
# everything the servers stored.
#
#   scripts/test_protocols.sh
set -eu
cd "$(dirname "$0")/.."

DIR=$(mktemp -d /tmp/forkz-protocols-test.XXXXXX)
PIDS=""
cleanup() {
    for pid in $PIDS; do kill "$pid" 2>/dev/null || true; done
    rm -rf "$DIR"
}
trap cleanup EXIT INT TERM

python3 -m venv "$DIR/venv"
"$DIR/venv/bin/pip" install --quiet --disable-pip-version-check pyftpdlib pyopenssl wsgidav cheroot "moto[server]"

mkdir -p "$DIR/ftp" "$DIR/dav"
openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj "/CN=forkz test" \
    -keyout "$DIR/cert.pem" -out "$DIR/cert.pem.crt" 2>/dev/null
cat "$DIR/cert.pem.crt" >> "$DIR/cert.pem"

cat > "$DIR/ftp.py" <<'PY'
import sys
from pyftpdlib.authorizers import DummyAuthorizer
from pyftpdlib.handlers import TLS_FTPHandler
from pyftpdlib.servers import FTPServer

root, cert = sys.argv[1], sys.argv[2]
authorizer = DummyAuthorizer()
authorizer.add_user("forkz", "secret", root, perm="elradfmwMT")
handler = TLS_FTPHandler
handler.certfile = cert
handler.authorizer = authorizer
handler.passive_ports = range(30000, 30100)
# TLS is offered, not required, so plain FTP can be tested too.
handler.tls_control_required = False
handler.tls_data_required = False
FTPServer(("127.0.0.1", 2121), handler).serve_forever()
PY
"$DIR/venv/bin/python" "$DIR/ftp.py" "$DIR/ftp" "$DIR/cert.pem" > "$DIR/ftp.log" 2>&1 &
PIDS="$PIDS $!"

cat > "$DIR/wsgidav.yaml" <<YAML
host: 127.0.0.1
port: 8080
provider_mapping:
    "/dav": "$DIR/dav"
http_authenticator:
    accept_basic: true
    accept_digest: false
    default_to_digest: false
simple_dc:
    user_mapping:
        "*":
            "forkz":
                password: "secret"
verbose: 1
YAML
"$DIR/venv/bin/wsgidav" --config "$DIR/wsgidav.yaml" > "$DIR/dav.log" 2>&1 &
PIDS="$PIDS $!"

"$DIR/venv/bin/moto_server" -H 127.0.0.1 -p 9555 > "$DIR/s3.log" 2>&1 &
PIDS="$PIDS $!"

# Wait for all three to listen.
for port in 2121 8080 9555; do
    for _ in $(seq 1 50); do
        nc -z 127.0.0.1 "$port" 2>/dev/null && break
        sleep 0.2
    done
done

status=0
FORKZ_TEST_FTP=127.0.0.1:2121 FORKZ_TEST_DAV=127.0.0.1:8080 FORKZ_TEST_S3=127.0.0.1:9555 \
    cargo test --test protocols_live -- --ignored --test-threads=1 "$@" || status=$?
if [ "$status" -ne 0 ]; then
    for log in ftp dav s3; do
        echo "--- $log.log (last lines)"
        tail -n 30 "$DIR/$log.log"
    done
fi
exit "$status"
