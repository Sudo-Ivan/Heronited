#!/usr/bin/env bash
# Live tests of the sharing crate against real servers: rclone serve
# webdav/sftp/ftp/s3 plus an unprivileged OpenSSH sshd, all on 127.0.0.1 high
# ports under /tmp. Everything it starts is killed on exit.
set -euo pipefail

WORK=/tmp/heronite-share-live
mkdir -p "$WORK"
cd "$(dirname "$0")/.."

RCLONE_VERSION=v1.75.2
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

# ---- rclone, verified against the release's SHA256SUMS ----
RCLONE="$WORK/rclone"
if [[ ! -x "$RCLONE" ]]; then
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
mkdir -p "$ROOT/bucket"
USER=liveuser
PASS=livepass123

pick_port() {
    python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()'
}

WEBDAV_PORT=$(pick_port)
SFTP_PORT=$(pick_port)
FTP_PORT=$(pick_port)
S3_PORT=$(pick_port)
SSHD_PORT=$(pick_port)

"$RCLONE" serve webdav "$ROOT" --addr "127.0.0.1:$WEBDAV_PORT" --user "$USER" --pass "$PASS" >/dev/null 2>&1 &
PIDS+=($!)
"$RCLONE" serve sftp "$ROOT" --addr "127.0.0.1:$SFTP_PORT" --user "$USER" --pass "$PASS" >/dev/null 2>&1 &
PIDS+=($!)
"$RCLONE" serve ftp "$ROOT" --addr "127.0.0.1:$FTP_PORT" --user "$USER" --pass "$PASS" >/dev/null 2>&1 &
PIDS+=($!)
"$RCLONE" serve s3 "$ROOT" --addr "127.0.0.1:$S3_PORT" --auth-key "liveaccess,$PASS" --no-auth >/dev/null 2>&1 &
PIDS+=($!)

# ---- real OpenSSH sshd, unprivileged ----
SSHD_SKIP=""
if [[ -x /usr/bin/sshd || -x /usr/sbin/sshd ]]; then
    SSHD_BIN=/usr/bin/sshd
    [[ -x /usr/sbin/sshd ]] && SSHD_BIN=/usr/sbin/sshd
    SSHDIR="$WORK/ssh"
    mkdir -p "$SSHDIR"
    ssh-keygen -q -t ed25519 -N "" -f "$SSHDIR/host_key" <<<y >/dev/null 2>&1 || true
    ssh-keygen -q -t ed25519 -N "" -f "$SSHDIR/user_key" <<<y >/dev/null 2>&1 || true
    cp "$SSHDIR/user_key.pub" "$SSHDIR/authorized_keys"
    SFTP_SERVER=/usr/lib/ssh/sftp-server
    [[ -x /usr/lib/openssh/sftp-server ]] && SFTP_SERVER=/usr/lib/openssh/sftp-server
    cat > "$SSHDIR/sshd_config" <<CONF
Port $SSHD_PORT
ListenAddress 127.0.0.1
HostKey $SSHDIR/host_key
AuthorizedKeysFile $SSHDIR/authorized_keys
PasswordAuthentication no
PubkeyAuthentication yes
PidFile $SSHDIR/sshd.pid
Subsystem sftp $SFTP_SERVER
StrictModes no
CONF
    "$SSHD_BIN" -f "$SSHDIR/sshd_config" -D -e >"$SSHDIR/sshd.log" 2>&1 &
    PIDS+=($!)
else
    SSHD_SKIP="no sshd installed"
fi

# ---- MinIO in podman, when it can run ----
MINIO_SKIP=""
if command -v podman >/dev/null 2>&1; then
    MINIO_PORT=$(pick_port)
    if podman image exists docker.io/minio/minio:latest 2>/dev/null || podman pull -q docker.io/minio/minio:latest >/dev/null 2>&1; then
        MINIO_CONTAINER=$(podman run -d --rm -p "127.0.0.1:$MINIO_PORT:9000" -e MINIO_ROOT_USER=liveaccess -e MINIO_ROOT_PASSWORD=livepass123 docker.io/minio/minio:latest server /data 2>/dev/null || true)
        if [[ -z "$MINIO_CONTAINER" ]]; then
            MINIO_SKIP="podman run failed"
        else
            sleep 4
        fi
    else
        MINIO_SKIP="podman cannot pull the MinIO image"
    fi
else
    MINIO_SKIP="podman not installed"
fi

sleep 2

export SHARE_LIVE_WEBDAV="http://127.0.0.1:$WEBDAV_PORT"
export SHARE_LIVE_SFTP_RCLONE="127.0.0.1:$SFTP_PORT"
export SHARE_LIVE_FTP="127.0.0.1:$FTP_PORT"
export SHARE_LIVE_S3_RCLONE="http://127.0.0.1:$S3_PORT"
export SHARE_LIVE_USER="$USER"
export SHARE_LIVE_PASS="$PASS"
[[ -z "$SSHD_SKIP" ]] && export SHARE_LIVE_SSHD="127.0.0.1:$SSHD_PORT" || true
export SHARE_LIVE_SSH_KEY="$WORK/ssh/user_key"
[[ -z "$MINIO_SKIP" ]] && export SHARE_LIVE_MINIO="http://127.0.0.1:${MINIO_PORT:-0}" || true

echo "== running the live tests =="
cargo test -p heronite-share --test live -- --ignored --test-threads=1 || true

echo
echo "== what ran and what was skipped =="
printf '%-22s %s\n' "rclone webdav" "ran on $WEBDAV_PORT"
printf '%-22s %s\n' "rclone sftp" "ran on $SFTP_PORT"
printf '%-22s %s\n' "rclone ftp" "ran on $FTP_PORT"
printf '%-22s %s\n' "rclone s3" "ran on $S3_PORT"
if [[ -z "$SSHD_SKIP" ]]; then
    printf '%-22s %s\n' "openssh sshd" "ran on $SSHD_PORT"
else
    printf '%-22s %s\n' "openssh sshd" "SKIP: $SSHD_SKIP"
fi
if [[ -z "$MINIO_SKIP" ]]; then
    printf '%-22s %s\n' "minio (real multipart)" "ran on ${MINIO_PORT:-?}"
else
    printf '%-22s %s\n' "minio (real multipart)" "SKIP: $MINIO_SKIP"
fi
printf '%-22s %s\n' "nextcloud OCS" "SKIP: mock only, a real Nextcloud is out of scope"
