#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: sudo deploy/install.sh [--bin-dir PATH] [--enable]

Without --bin-dir, the installer builds locked release binaries from this checkout.
Existing /etc/avian/avian.toml and secret files are never overwritten.
--enable starts both services after installation; provision configuration and keys first.
EOF
}

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
BIN_SOURCE=""
ENABLE_SERVICES=0

while (($#)); do
    case "$1" in
        --bin-dir)
            [[ $# -ge 2 ]] || { usage >&2; exit 2; }
            BIN_SOURCE="$2"
            shift 2
            ;;
        --enable)
            ENABLE_SERVICES=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            printf 'Unknown argument: %s\n' "$1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

[[ "$(uname -s)" == "Linux" ]] || { printf 'AVIAN systemd installation requires Linux.\n' >&2; exit 1; }
[[ "$EUID" -eq 0 ]] || { printf 'Run this installer as root.\n' >&2; exit 1; }

if [[ -z "$BIN_SOURCE" ]]; then
    command -v cargo >/dev/null || { printf 'cargo is required when --bin-dir is omitted.\n' >&2; exit 1; }
    cargo build --manifest-path "$REPO_ROOT/Cargo.toml" --release --locked -p mesh-agent --bins
    BIN_SOURCE="$REPO_ROOT/target/release"
fi

for binary in mesh-agent avianctl avian-link-monitor; do
    [[ -x "$BIN_SOURCE/$binary" ]] || { printf 'Missing executable: %s/%s\n' "$BIN_SOURCE" "$binary" >&2; exit 1; }
done

getent group avian >/dev/null || groupadd --system avian
if ! id -u avian >/dev/null 2>&1; then
    useradd --system --gid avian --home-dir /var/lib/avian --create-home --shell /usr/sbin/nologin avian
fi

install -d -m 0750 -o root -g avian /etc/avian
install -d -m 0750 -o avian -g avian /var/lib/avian
install -d -m 0755 -o root -g root /usr/local/bin /usr/local/share/doc/avian

for binary in mesh-agent avianctl avian-link-monitor; do
    install -m 0755 -o root -g root "$BIN_SOURCE/$binary" "/usr/local/bin/$binary"
done

install -m 0644 -o root -g root "$REPO_ROOT/config/avian.toml.example" \
    /usr/local/share/doc/avian/avian.toml.example
install -m 0640 -o root -g avian "$REPO_ROOT/config/avian.toml.example" \
    /etc/avian/avian.toml.example
if [[ ! -e /etc/avian/avian.toml ]]; then
    install -m 0640 -o root -g avian "$REPO_ROOT/config/avian.toml.example" \
        /etc/avian/avian.toml
    printf 'Created /etc/avian/avian.toml; provision it and formation.key before enabling services.\n'
else
    printf 'Preserved existing /etc/avian/avian.toml.\n'
fi

install -m 0644 -o root -g root "$REPO_ROOT/deploy/systemd/avian-mesh-agent.service" \
    /etc/systemd/system/avian-mesh-agent.service
install -m 0644 -o root -g root "$REPO_ROOT/deploy/systemd/avian-link-monitor.service" \
    /etc/systemd/system/avian-link-monitor.service

if id -u rolex >/dev/null 2>&1; then
    usermod -a --groups avian rolex
fi

systemctl daemon-reload
if ((ENABLE_SERVICES)); then
    systemctl enable --now avian-mesh-agent.service avian-link-monitor.service
else
    printf 'Services installed but not enabled. Use --enable after provisioning configuration and keys.\n'
fi
