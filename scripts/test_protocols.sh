#!/bin/sh
# Runs the live FTP/FTPS, WebDAV, S3, Azure, and Box tests against throwaway
# servers on 127.0.0.1: pyftpdlib (FTP with explicit TLS, a self-signed
# certificate; two more for FXP, one of which refuses it), WsgiDAV, moto (an
# S3 emulator), Azurite (if Node is installed), and a stand-in for Box's API
# (scripts/box_mock.py). They run from a Python virtual
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

mkdir -p "$DIR/ftp" "$DIR/ftp2" "$DIR/ftp3" "$DIR/dav"
openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj "/CN=forkz test" \
    -keyout "$DIR/cert.pem" -out "$DIR/cert.pem.crt" 2>/dev/null
cat "$DIR/cert.pem.crt" >> "$DIR/cert.pem"

cat > "$DIR/ftp.py" <<'PY'
import sys
from pyftpdlib.authorizers import DummyAuthorizer
from pyftpdlib.handlers import TLS_FTPHandler
from pyftpdlib.servers import FTPServer

root, cert, port = sys.argv[1], sys.argv[2], int(sys.argv[3])
refuse_fxp = len(sys.argv) > 4

class Handler(TLS_FTPHandler):
    def ftp_PORT(self, line):
        # As many servers do: no data connections to other machines.
        if refuse_fxp:
            self.respond("500 PORT is disabled.")
            return
        TLS_FTPHandler.ftp_PORT(self, line)

authorizer = DummyAuthorizer()
authorizer.add_user("forkz", "secret", root, perm="elradfmwMT")
handler = Handler
handler.certfile = cert
handler.authorizer = authorizer
handler.passive_ports = range(30000 + (port - 2121) * 100, 30100 + (port - 2121) * 100)
# TLS is offered, not required, so plain FTP can be tested too.
handler.tls_control_required = False
handler.tls_data_required = False
FTPServer(("127.0.0.1", port), handler).serve_forever()
PY
"$DIR/venv/bin/python" "$DIR/ftp.py" "$DIR/ftp" "$DIR/cert.pem" 2121 > "$DIR/ftp.log" 2>&1 &
PIDS="$PIDS $!"
"$DIR/venv/bin/python" "$DIR/ftp.py" "$DIR/ftp2" "$DIR/cert.pem" 2122 > "$DIR/ftp2.log" 2>&1 &
PIDS="$PIDS $!"
"$DIR/venv/bin/python" "$DIR/ftp.py" "$DIR/ftp3" "$DIR/cert.pem" 2123 refuse-fxp > "$DIR/ftp3.log" 2>&1 &
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

# A stand-in for Box's API (Python's standard library only).
"$DIR/venv/bin/python" scripts/box_mock.py 9777 > "$DIR/box.log" 2>&1 &
PIDS="$PIDS $!"

# Azurite (Microsoft's Azure Storage emulator), if Node is installed.
AZURE=""
if command -v npx >/dev/null 2>&1; then
    npx --yes --package=azurite azurite-blob --blobHost 127.0.0.1 --blobPort 10000 --location "$DIR/azurite" --silent \
        > "$DIR/azure.log" 2>&1 &
    PIDS="$PIDS $!"
    AZURE=127.0.0.1:10000
fi

# Wait for all of them to listen.
for port in 2121 2122 2123 8080 9555 9777 ${AZURE:+10000}; do
    for _ in $(seq 1 50); do
        nc -z 127.0.0.1 "$port" 2>/dev/null && break
        sleep 0.2
    done
done

status=0
FORKZ_TEST_FTP=127.0.0.1:2121 FORKZ_TEST_FTP2=127.0.0.1:2122 FORKZ_TEST_FTP_NO_FXP=127.0.0.1:2123 \
FORKZ_TEST_DAV=127.0.0.1:8080 FORKZ_TEST_S3=127.0.0.1:9555 FORKZ_TEST_AZURE="$AZURE" FORKZ_TEST_BOX=127.0.0.1:9777 \
    cargo test --test protocols_live -- --ignored --test-threads=1 "$@" || status=$?
if [ "$status" -ne 0 ]; then
    for log in ftp ftp2 ftp3 dav s3 azure box; do
        echo "--- $log.log (last lines)"
        tail -n 30 "$DIR/$log.log"
    done
fi
exit "$status"
