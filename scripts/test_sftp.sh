#!/bin/sh
# Runs the live SFTP tests against a throwaway SSH server: OpenSSH's sshd,
# run as you on 127.0.0.1:2222 with its own keys (key login only), in a
# temporary folder that is removed afterwards. Nothing in ~/.ssh is used or
# changed.
#
#   scripts/test_sftp.sh
set -eu
cd "$(dirname "$0")/.."

DIR=$(mktemp -d /tmp/forkz-sftp-test.XXXXXX)
cleanup() {
    [ -f "$DIR/sshd.pid" ] && kill "$(cat "$DIR/sshd.pid")" 2>/dev/null || true
    rm -rf "$DIR"
}
trap cleanup EXIT INT TERM

mkdir -p "$DIR/remote"
ssh-keygen -q -t ed25519 -N "" -f "$DIR/host_ed25519"
ssh-keygen -q -t ed25519 -N "" -f "$DIR/client_key" -C forkz-test
ssh-keygen -q -t ed25519 -N "hunter2" -f "$DIR/client_key_pass" -C forkz-test-pass
cat "$DIR/client_key.pub" "$DIR/client_key_pass.pub" > "$DIR/authorized_keys"
cat > "$DIR/sshd_config" <<CONFIG
Port 2222
ListenAddress 127.0.0.1
HostKey $DIR/host_ed25519
PidFile $DIR/sshd.pid
AuthorizedKeysFile $DIR/authorized_keys
StrictModes no
UsePAM no
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
Subsystem sftp internal-sftp
CONFIG
/usr/sbin/sshd -f "$DIR/sshd_config" -E "$DIR/sshd.log"
sleep 1

FORKZ_TEST_SFTP=127.0.0.1:2222 FORKZ_TEST_KEY="$DIR/client_key" FORKZ_TEST_KEY_PASS="$DIR/client_key_pass" \
    FORKZ_TEST_REMOTE="$DIR/remote" cargo test --test sftp_live -- --ignored --test-threads=1
