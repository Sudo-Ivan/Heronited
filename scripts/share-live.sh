#!/usr/bin/env bash
# Live tests of the sharing crate against real servers: rclone serve
# webdav/sftp/ftp/s3, a real OpenSSH sshd (unprivileged, own config and keys)
# and MinIO in podman when it can run. Everything binds 127.0.0.1 high ports,
# lives under /tmp/heronite-share-live and is killed on exit.
#
# Exports for crates/share/tests/live.rs (documented there too):
#   SHARE_LIVE_WEBDAV SHARE_LIVE_FTP SHARE_LIVE_FTPS SHARE_LIVE_FTPS_CA
#   SHARE_LIVE_SFTP_RCLONE SHARE_LIVE_SSHD SHARE_LIVE_SSH_KEY SHARE_LIVE_SSH_ROOT
#   SHARE_LIVE_S3_RCLONE SHARE_LIVE_MINIO SHARE_LIVE_USER SHARE_LIVE_PASS
#   SHARE_LIVE_S3_KEY SHARE_LIVE_S3_SECRET
set -euo pipefail

WORK=/tmp/heronite-share-live
mkdir -p "$WORK"
cd "$(dirname "$0")/.."

RCLONE_VERSION=v1.75.2   # same pin as crates/sync/src/rclone.rs
PIDS=()
cleanup() {
    for pid in "${PIDS[@]:-}"; do
        kill "$pid" 2>/dev/null || true
    done
    if [[ -n "${MINIO_CONTAINER:-}" ]]; then
        podman rm -f "$MINIO_CONTAINER" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT

pick_port() {
    python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()'
}

# wait until a TCP port answers, 10 s budget
wait_port() {
    python3 - "$1" <<'PY'
import socket, sys, time
port = int(sys.argv[1])
for _ in range(200):
    try:
        s = socket.create_connection(("127.0.0.1", port), timeout=0.2)
        s.close()
        sys.exit(0)
    except OSError:
        time.sleep(0.05)
sys.exit(1)
PY
}

# ---- rclone, verified against the release's official SHA256SUMS ----
RCLONE="$WORK/rclone"
verify_rclone() {
    [[ -x "$RCLONE" ]] || return 1
    [[ -f "$WORK/SHA256SUMS" ]] || return 1
    # the zip is verified at download time; a present binary is re-used
    return 0
}
if ! verify_rclone; then
    echo "downloading rclone $RCLONE_VERSION"
    curl -fsSL "https://downloads.rclone.org/$RCLONE_VERSION/SHA256SUMS" -o "$WORK/SHA256SUMS"
    curl -fsSL "https://downloads.rclone.org/$RCLONE_VERSION/rclone-$RCLONE_VERSION-linux-amd64.zip" -o "$WORK/rclone.zip"
    want=$(grep "rclone-$RCLONE_VERSION-linux-amd64.zip" "$WORK/SHA256SUMS" | awk '{print $1}')
    got=$(sha256sum "$WORK/rclone.zip" | awk '{print $1}')
    if [[ "$want" != "$got" ]]; then
        echo "rclone download hash mismatch" >&2
        exit 1
    fi
    unzip -o -q "$WORK/rclone.zip" -d "$WORK"
    mv "$WORK/rclone-$RCLONE_VERSION-linux-amd64/rclone" "$RCLONE"
fi

ROOT="$WORK/root"
rm -rf "$ROOT"
mkdir -p "$ROOT/bucket"
LIVEUSER=liveuser
PASS=livepass123
S3KEY=liveaccess

WEBDAV_PORT=$(pick_port)
SFTP_PORT=$(pick_port)
FTP_PORT=$(pick_port)
FTPS_PORT=$(pick_port)
S3_PORT=$(pick_port)
SSHD_PORT=$(pick_port)

declare -A SKIP
for name in webdav sftp ftp ftps s3 sshd minio; do SKIP[$name]=""; done

start_rclone() { # name port extra-args...
    local name=$1 port=$2
    shift 2
    "$RCLONE" "$@" >"$WORK/$name.log" 2>&1 &
    PIDS+=($!)
    if ! wait_port "$port"; then
        SKIP[$name]="did not start, see $WORK/$name.log"
        return 1
    fi
    return 0
}

# a short dir cache: files written through the other rclone servers must
# become visible for the readback within a second
start_rclone webdav "$WEBDAV_PORT" serve webdav "$ROOT" --addr "127.0.0.1:$WEBDAV_PORT" --user "$LIVEUSER" --pass "$PASS" --dir-cache-time 500ms || true
start_rclone sftp "$SFTP_PORT" serve sftp "$ROOT" --addr "127.0.0.1:$SFTP_PORT" --user "$LIVEUSER" --pass "$PASS" || true
start_rclone ftp "$FTP_PORT" serve ftp "$ROOT" --addr "127.0.0.1:$FTP_PORT" --user "$LIVEUSER" --pass "$PASS" || true
start_rclone s3 "$S3_PORT" serve s3 "$ROOT" --addr "127.0.0.1:$S3_PORT" --auth-key "$S3KEY,$PASS" || true

# FTPS: same ftp server with a self-signed certificate. rclone speaks
# implicit FTPS here (verified with openssl s_client), not explicit AUTH TLS.
FTPS_CERT="$WORK/ftps-cert.pem"
FTPS_KEY="$WORK/ftps-key.pem"
openssl req -x509 -newkey rsa:2048 -keyout "$FTPS_KEY" -out "$FTPS_CERT" -days 2 -nodes -subj "/CN=127.0.0.1" -addext "subjectAltName=IP:127.0.0.1" -addext "basicConstraints=critical,CA:FALSE" >/dev/null 2>&1
# not started on purpose: rclone's FTP TLS is implicit and the data channel
# stays plaintext, so a conforming client cannot round-trip a file through it
SKIP[ftps]="rclone serve ftp TLS leaves the data channel plaintext (goftp limitation)"

# ---- real OpenSSH sshd, unprivileged, key auth only ----
# (password auth would need a system user or PAM, neither exists unprivileged)
SSHROOT="$WORK/sshroot"
rm -rf "$SSHROOT"
mkdir -p "$SSHROOT"
if [[ -x /usr/sbin/sshd ]]; then
    SSHD_BIN=/usr/sbin/sshd
    SSHDIR="$WORK/ssh"
    mkdir -p "$SSHDIR"
    [[ -f "$SSHDIR/host_key" ]] || ssh-keygen -q -t ed25519 -N "" -f "$SSHDIR/host_key" >/dev/null 2>&1
    [[ -f "$SSHDIR/host_key_rsa" ]] || ssh-keygen -q -t rsa -b 3072 -N "" -f "$SSHDIR/host_key_rsa" >/dev/null 2>&1
    rm -f "$SSHDIR/user_key" "$SSHDIR/user_key.pub"
    ssh-keygen -q -t ed25519 -N "" -f "$SSHDIR/user_key" >/dev/null 2>&1
    cp "$SSHDIR/user_key.pub" "$SSHDIR/authorized_keys"
    SFTP_SERVER=/usr/lib/ssh/sftp-server
    [[ -x /usr/lib/openssh/sftp-server ]] && SFTP_SERVER=/usr/lib/openssh/sftp-server
    cat > "$SSHDIR/sshd_config" <<CONF
Port $SSHD_PORT
ListenAddress 127.0.0.1
HostKey $SSHDIR/host_key
HostKey $SSHDIR/host_key_rsa
AuthorizedKeysFile $SSHDIR/authorized_keys
PasswordAuthentication no
PubkeyAuthentication yes
UsePAM no
StrictModes no
PidFile $SSHDIR/sshd.pid
Subsystem sftp $SFTP_SERVER
CONF
    "$SSHD_BIN" -f "$SSHDIR/sshd_config" -D -e >"$SSHDIR/sshd.log" 2>&1 &
    PIDS+=($!)
    if ! wait_port "$SSHD_PORT"; then
        SKIP[sshd]="did not start, see $SSHDIR/sshd.log"
    fi
else
    SKIP[sshd]="no sshd installed"
fi

# ---- MinIO in podman, when it can run (120 s pull budget) ----
MINIO_PORT=""
if command -v podman >/dev/null 2>&1; then
    MINIO_PORT=$(pick_port)
    IMAGE=""
    for candidate in docker.io/minio/minio:latest quay.io/minio/minio:latest; do
        if podman image exists "$candidate" 2>/dev/null; then
            IMAGE=$candidate
            break
        fi
        if timeout 120 podman pull -q "$candidate" >/dev/null 2>&1; then
            IMAGE=$candidate
            break
        fi
    done
    if [[ -z "$IMAGE" ]]; then
        SKIP[minio]="no image pulled within 120 s"
    else
        MINIO_CONTAINER=$(podman run -d --rm -p "127.0.0.1:$MINIO_PORT:9000" -e MINIO_ROOT_USER=liveaccess -e MINIO_ROOT_PASSWORD=livepass123 "$IMAGE" server /data 2>/dev/null || true)
        if [[ -z "$MINIO_CONTAINER" ]]; then
            SKIP[minio]="podman run failed"
        elif ! wait_port "$MINIO_PORT"; then
            SKIP[minio]="did not answer on $MINIO_PORT"
        fi
    fi
else
    SKIP[minio]="podman not installed"
fi

[[ -z "${SKIP[webdav]}" ]] && export SHARE_LIVE_WEBDAV="http://127.0.0.1:$WEBDAV_PORT" || true
[[ -z "${SKIP[sftp]}" ]] && export SHARE_LIVE_SFTP_RCLONE="127.0.0.1:$SFTP_PORT" || true
[[ -z "${SKIP[ftp]}" ]] && export SHARE_LIVE_FTP="127.0.0.1:$FTP_PORT" || true
[[ -z "${SKIP[ftps]}" ]] && export SHARE_LIVE_FTPS="127.0.0.1:$FTPS_PORT" || true
export SHARE_LIVE_FTPS_CA="$FTPS_CERT"
[[ -z "${SKIP[s3]}" ]] && export SHARE_LIVE_S3_RCLONE="http://127.0.0.1:$S3_PORT" || true
[[ -z "${SKIP[sshd]}" ]] && export SHARE_LIVE_SSHD="127.0.0.1:$SSHD_PORT" || true
export SHARE_LIVE_SSH_KEY="$WORK/ssh/user_key"
export SHARE_LIVE_SSH_ROOT="$SSHROOT"
[[ -z "${SKIP[minio]}" && -n "$MINIO_PORT" ]] && export SHARE_LIVE_MINIO="http://127.0.0.1:$MINIO_PORT" || true
export SHARE_LIVE_USER="$LIVEUSER"
export SHARE_LIVE_PASS="$PASS"
export SHARE_LIVE_S3_KEY="$S3KEY"
export SHARE_LIVE_S3_SECRET="$PASS"

echo "== running the live tests =="
LOG="$WORK/live-tests.log"
cargo test -p heronite-share --test live -- --ignored --test-threads=1 --nocapture >"$LOG" 2>&1 || true

echo
echo "== live results =="
printf '%-24s %-6s %s\n' TEST RESULT DETAIL
TESTS="live_webdav_rclone live_ftp_rclone live_ftps_rclone live_sftp_rclone live_sftp_openssh_key live_s3_rclone live_s3_minio_multipart"
FAILED=0
for testname in $TESTS; do
    line=$(grep "^test $testname .*LIVE-RESULT " "$LOG" | head -1 || true)
    if [[ -n "$line" ]]; then
        after=${line#*LIVE-RESULT }
        # after is "<short-name> <RESULT> [detail...]"
        result=$(echo "$after" | awk '{print $2}')
        detail=$(echo "$after" | cut -d' ' -f3-)
        [[ "$detail" == "$result" ]] && detail=""
        printf '%-24s %-6s %s\n' "$testname" "$result" "$detail"
    elif grep -q "^test $testname ... FAILED" "$LOG"; then
        printf '%-24s %-6s %s\n' "$testname" FAIL "see $LOG"
        FAILED=1
    elif grep -q "^test $testname ... ok" "$LOG"; then
        printf '%-24s %-6s %s\n' "$testname" PASS ""
    else
        printf '%-24s %-6s %s\n' "$testname" FAIL "no result line, see $LOG"
        FAILED=1
    fi
done
printf '%-24s %-6s %s\n' nextcloud-ocs SKIP "mock only, a real Nextcloud is out of scope"
echo
if grep -q "^test .* FAILED" "$LOG" || [[ $FAILED -eq 1 ]]; then
    echo "some live tests FAILED; full log: $LOG"
    grep -A6 'panicked' "$LOG" | head -30 || true
    exit 1
fi
echo "all live tests passed or were skipped with a reason; full log: $LOG"
