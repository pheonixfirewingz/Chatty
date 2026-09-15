#!/usr/bin/env bash
# Run on the broker host as the checkout owner; sudo is used for system services.
set -Eeuo pipefail

# Parse the complete deployment before pulling: Git may update this script too.
main() {
repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
service=chatty-broker.service
binary=''
scope=auto
service_ctl=(systemctl)

usage() {
    printf '%s\n' \
        'Usage: scripts/deploy-broker.sh [--user|--system] [--repo DIR] [--service NAME] [--binary PATH]' \
        '' \
        'Run ONLY on the broker host, as the Git checkout owner.' \
        'Requires a clean checkout, an upstream branch, Rust, and an installed service.' \
        'Detects a system service first, then a per-user service; --user/--system override this.' \
        'System deployments require sudo; per-user deployments use the current account.' \
        'Pulls with --ff-only, builds from scratch, backs up and replaces the broker,' \
        'then starts the service and checks it for 10 seconds.' \
        'Back up the broker database before deploying; migrations cannot be rolled back automatically.'
}

die() {
    printf 'Deploy failed: %s\n' "$*" >&2
    exit 1
}

while (($#)); do
    case "$1" in
        --repo|--service|--binary)
            (($# >= 2)) || die "Missing value for $1"
            case "$1" in
                --repo) repo_dir=$2 ;;
                --service) service=$2 ;;
                --binary) binary=$2 ;;
            esac
            shift 2
            ;;
        --user) scope=user; shift ;;
        --system) scope=system; shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "Unknown argument: $1" ;;
    esac
done

[[ "$service" =~ ^[a-zA-Z0-9][a-zA-Z0-9_.@-]*$ ]] || die 'Invalid service name'
[[ "$service" == *.service ]] || service+=.service
[[ -z "$binary" || ( "$binary" == /* && "$binary" != */ ) ]] || die '--binary must be an absolute file path'
for command in git cargo rustc systemctl install mktemp cp mv rm flock sed date sleep; do
    command -v "$command" >/dev/null || die "Required command not found: $command"
done

repo_dir=$(cd -- "$repo_dir" && pwd)
[[ -f "$repo_dir/crates/chatty-broker/Cargo.toml" ]] || die 'Not a Chatty checkout'
cd -- "$repo_dir"
lock_file=$(git rev-parse --git-path chatty-deploy.lock)
exec 9>"$lock_file"
flock -n 9 || die 'Another deployment is using this checkout'
[[ -z "$(git status --porcelain)" ]] || die 'Checkout has local changes; commit or move them before deploying'
git symbolic-ref -q HEAD >/dev/null || die 'Checkout is detached; switch to the deployment branch'
git rev-parse --verify '@{upstream}' >/dev/null || die 'Current branch has no upstream'

service_loaded() {
    [[ "$("$@" show --property=LoadState --value "$service" 2>/dev/null)" == loaded ]]
}
if [[ "$scope" == auto ]]; then
    if service_loaded systemctl; then
        scope=system
    elif service_loaded systemctl --user; then
        scope=user
    else
        die "Service $service is not installed in the system or current user's service manager; install a service from packaging/ or specify --service NAME"
    fi
fi
privileged=()
install_owner=()
if [[ "$scope" == user ]]; then
    service_ctl+=(--user)
    binary=${binary:-"$HOME/.local/libexec/chatty/chatty-broker"}
else
    binary=${binary:-/usr/local/bin/chatty-broker}
    install_owner=(-o root -g root)
    if ((EUID != 0)); then
        command -v sudo >/dev/null || die 'sudo is required to manage the system service'
        privileged=(sudo)
    fi
fi
service_loaded "${service_ctl[@]}" || die "$scope service not found: $service"
configured_exec=$("${service_ctl[@]}" show --property=ExecStart --value "$service")
[[ "$configured_exec" == *"path=$binary ;"* ]] || die "Service ExecStart does not match $binary; use --binary with its installed path"
printf 'Deploying %s service %s to %s\n' "$scope" "$service" "$binary"
[[ -f "$binary" && ! -L "$binary" ]] || die 'Installed binary must be a regular file, not a symlink'

build_dir=''
staged_binary=''
backup_binary=''
service_stopped=false
binary_replaced=false
deployment_complete=false

cleanup() {
    local result=$?
    trap - EXIT
    if [[ "$deployment_complete" != true && "$service_stopped" == true ]]; then
        if [[ "$binary_replaced" == false ]]; then
            printf 'Replacement failed; restarting the unchanged broker.\n' >&2
            "${privileged[@]}" "${service_ctl[@]}" start "$service" || true
        else
            "${privileged[@]}" "${service_ctl[@]}" stop "$service" || true
            printf 'New broker did not stay running; service left stopped.\nBackup binary: %s\nInspect: journalctl %s -u %s -n 100 --no-pager\n' \
                "$backup_binary" "--$scope" "$service" >&2
            printf 'Check database migration state before attempting rollback.\n' >&2
        fi
    fi
    if [[ -n "$staged_binary" ]]; then
        "${privileged[@]}" rm -f -- "$staged_binary" || true
    fi
    if [[ -n "$build_dir" ]]; then
        rm -rf -- "$build_dir" || true
    fi
    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

printf 'Pulling the upstream branch in %s…\n' "$repo_dir"
git pull --ff-only
revision=$(git rev-parse HEAD)
build_dir=$(mktemp -d "${TMPDIR:-/tmp}/chatty-deploy.XXXXXXXX")
host_target=$(rustc -vV | sed -n 's/^host: //p')
[[ -n "$host_target" ]] || die 'Cannot determine the Rust host target'
printf 'Building broker revision %s from scratch for %s…\n' "$revision" "$host_target"
cargo build --locked --release --package chatty-broker --bin chatty-broker \
    --target "$host_target" --target-dir "$build_dir"
built_binary="$build_dir/$host_target/release/chatty-broker"
[[ -x "$built_binary" ]] || die 'Fresh build did not produce a broker executable'

# Stage on the installed binary's filesystem so the replacement is atomic.
if ((${#privileged[@]})); then
    sudo -v
fi
staged_binary=$("${privileged[@]}" mktemp "${binary}.deploy.XXXXXXXX")
"${privileged[@]}" install "${install_owner[@]}" -m 0755 -- "$built_binary" "$staged_binary"
backup_binary="${binary}.backup.$(date -u +%Y%m%dT%H%M%SZ).$$"
"${privileged[@]}" cp --preserve=mode,ownership,timestamps -- "$binary" "$backup_binary"
printf 'Previous binary backed up to %s\nStopping %s…\n' "$backup_binary" "$service"
"${privileged[@]}" "${service_ctl[@]}" stop "$service"
service_stopped=true
"${privileged[@]}" mv -fT -- "$staged_binary" "$binary"
binary_replaced=true
staged_binary=''

printf 'Starting %s…\n' "$service"
"${privileged[@]}" "${service_ctl[@]}" start "$service"
started_pid=$("${service_ctl[@]}" show --property=MainPID --value "$service")
[[ "$started_pid" =~ ^[1-9][0-9]*$ ]] || die 'Service has no running main process'
for ((second=0; second<10; second++)); do
    sleep 1
    "${service_ctl[@]}" is-active --quiet "$service" || die 'Service stopped after startup'
    [[ "$("${service_ctl[@]}" show --property=MainPID --value "$service")" == "$started_pid" ]] || die 'Broker restarted during the startup check'
done
deployment_complete=true
printf 'Deployed %s to %s. %s is running (PID %s).\n' "$revision" "$binary" "$service" "$started_pid"
printf 'Use a GUI built from the same revision.\n'
}

main "$@"
