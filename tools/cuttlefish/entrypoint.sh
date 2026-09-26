#!/usr/bin/env bash
# Boots the Cuttlefish device, then keeps the container alive so tests can be run
# repeatedly with `docker exec` instead of paying for a fresh boot each time.
#
# /cf is bind-mounted from the host (.cuttlefish/): device images, logs and a
# state file survive the container.
set -euo pipefail

state() { echo "$1" >/cf/state; echo "[cf] state: $1"; }

# Container-local permission changes on the passed-through device nodes.
chmod 666 /dev/kvm /dev/net/tun /dev/vhost-net /dev/vhost-vsock

# Bridges, taps and dnsmasq for the guest's virtio-net, inside this container's
# network namespace only.
service cuttlefish-host-resources start

for g in kvm cvdnetwork render; do groupadd -f "$g"; done
id cf >/dev/null 2>&1 || useradd -m -u "${HOST_UID:?}" -G kvm,cvdnetwork,render cf

mkdir -p /cf/device /cf/home /cf/xfer
chown cf /cf /cf/device /cf/home /cf/xfer

as_cf() { runuser -u cf -- env PATH="$PATH" HOME=/cf/home "$@"; }

stop_device() {
    state stopping
    as_cf env HOME=/cf/device /cf/device/bin/stop_cvd -wait_for_launcher=40 || true
    state stopped
    exit 0
}
trap stop_device TERM INT

if [ ! -x /cf/device/bin/launch_cvd ] || [ ! -f /cf/device/.fetched-"$CUTTLEFISH_BUILD" ]; then
    state fetching
    as_cf cvd fetch \
        --default_build="${CUTTLEFISH_BUILD}/${CUTTLEFISH_TARGET}" \
        --host_package_build="${CUTTLEFISH_BUILD}/${CUTTLEFISH_HOST_TARGET}" \
        --target_directory=/cf/device
    as_cf touch /cf/device/.fetched-"$CUTTLEFISH_BUILD"
fi

if [ "${CF_FETCH_ONLY:-0}" = 1 ]; then
    state fetched
    exit 0
fi

state booting
start=$(date +%s)
# -daemon returns only once the guest has finished booting (~20 min under TCG).
if ! as_cf env HOME=/cf/device timeout "${CUTTLEFISH_BOOT_TIMEOUT:-60m}" /cf/device/bin/launch_cvd \
    -daemon \
    -vm_manager="$CUTTLEFISH_VM_MANAGER" \
    -enable_sandbox=false \
    -report_anonymous_usage_stats=no \
    -verbosity=INFO \
    -enable_audio=false \
    -enable_bootanimation=false \
    -cpus="$CUTTLEFISH_CPUS" \
    -memory_mb="$CUTTLEFISH_MEMORY_MB"; then
    state failed
    exit 1
fi
echo "[cf] boot took $(( $(date +%s) - start ))s"

as_cf adb connect "$ANDROID_SERIAL"
as_cf adb -s "$ANDROID_SERIAL" wait-for-device
{
    echo "release=$(as_cf adb -s "$ANDROID_SERIAL" shell getprop ro.build.version.release)"
    echo "sdk=$(as_cf adb -s "$ANDROID_SERIAL" shell getprop ro.build.version.sdk)"
    echo "abi=$(as_cf adb -s "$ANDROID_SERIAL" shell getprop ro.product.cpu.abi)"
    echo "selinux=$(as_cf adb -s "$ANDROID_SERIAL" shell getenforce)"
} | tr -d '\r' | tee /cf/device-info
state ready

sleep infinity &
wait $!
