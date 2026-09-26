# Runs an arm64 Cuttlefish Android device under QEMU emulation on an x86_64 host.
# Recipe follows rust-lang/libc's CI (ci/cuttlefish-setup.sh, ci/cuttlefish-entrypoint.sh).
FROM debian:13

RUN apt-get update && apt-get install -y --no-install-recommends \
        adb \
        ca-certificates \
        curl \
        file \
        libasound2t64 \
        libpulse0 \
        procps \
        unzip \
    && rm -rf /var/lib/apt/lists/*

# Cuttlefish host packages (provides cvd and cuttlefish-host-resources).
# The fetched x86_64 host package brings its own qemu-system-aarch64, which links
# against libasound/libpulse (installed above).
RUN curl -fsSL https://us-apt.pkg.dev/doc/repo-signing-key.gpg \
        -o /etc/apt/trusted.gpg.d/artifact-registry.asc \
    && echo "deb https://us-apt.pkg.dev/projects/android-cuttlefish-artifacts android-cuttlefish main" \
        >/etc/apt/sources.list.d/artifact-registry.list \
    && apt-get update && apt-get install -y cuttlefish-base cuttlefish-user \
    && rm -rf /var/lib/apt/lists/*

# Pinned drop (Android 17). The arm64 device images run on x86_64 host tools from
# the same drop; crosvm can't run a foreign-architecture guest, qemu_cli can.
ENV CUTTLEFISH_BUILD=15660610 \
    CUTTLEFISH_TARGET=aosp_cf_arm64_only_phone-userdebug \
    CUTTLEFISH_HOST_TARGET=aosp_cf_x86_64_only_phone-userdebug \
    CUTTLEFISH_VM_MANAGER=qemu_cli \
    CUTTLEFISH_CPUS=4 \
    CUTTLEFISH_MEMORY_MB=4096 \
    ANDROID_SERIAL=127.0.0.1:6520

COPY entrypoint.sh /usr/local/bin/cf-entrypoint
ENTRYPOINT ["/usr/local/bin/cf-entrypoint"]
