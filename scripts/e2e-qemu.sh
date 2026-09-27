#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

KERNEL=${KERNEL:-$(ls /boot/vmlinuz-* 2>/dev/null | sort -V | tail -1 || true)}
BUSYBOX=${BUSYBOX:-/bin/busybox}
QEMU=${QEMU:-qemu-system-x86_64}
WORKDIR=${WORKDIR:-/tmp/patpans-e2e}
TARGET=x86_64-unknown-linux-musl

command -v cargo >/dev/null || { echo "error: cargo is not in PATH" >&2; exit 1; }
command -v "$QEMU" >/dev/null || { echo "error: $QEMU is missing (apt install qemu-system-x86)" >&2; exit 1; }
command -v cpio >/dev/null || { echo "error: cpio is missing (apt install cpio)" >&2; exit 1; }
[ -n "$KERNEL" ] && [ -e "$KERNEL" ] || { echo "error: no kernel image, set KERNEL= or install linux-image-generic" >&2; exit 1; }
[ -x "$BUSYBOX" ] || { echo "error: $BUSYBOX is missing (apt install busybox-static)" >&2; exit 1; }

rustup target list --installed | grep -q "^$TARGET$" || rustup target add "$TARGET"
cargo build --release --target "$TARGET" --test linux_uinput_e2e

TESTBIN=$(find "target/$TARGET" -path '*/out/linux_uinput_e2e-*' -type f -perm -u+x ! -name '*.d' | head -1)
[ -n "$TESTBIN" ] || { echo "error: e2e test binary not found" >&2; exit 1; }

rm -rf "$WORKDIR"
mkdir -p "$WORKDIR/rootfs/bin" "$WORKDIR/rootfs/proc" "$WORKDIR/rootfs/sys" "$WORKDIR/rootfs/dev" "$WORKDIR/rootfs/tmp"
if ! cp "$KERNEL" "$WORKDIR/vmlinuz" 2>/dev/null; then
    sudo cp "$KERNEL" "$WORKDIR/vmlinuz"
    sudo chown "$(id -u):$(id -g)" "$WORKDIR/vmlinuz"
fi
cp -L "$BUSYBOX" "$WORKDIR/rootfs/bin/busybox"
for applet in $("$BUSYBOX" --list); do
    [ "$applet" = "busybox" ] && continue
    ln -sf busybox "$WORKDIR/rootfs/bin/$applet"
done
cp "$TESTBIN" "$WORKDIR/rootfs/test"

cat > "$WORKDIR/rootfs/init" <<'EOF'
#!/bin/sh
mount -t proc proc /proc
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev
echo "E2E: kernel $(uname -r)"
echo "E2E: uinput entries in /proc/misc: $(grep -ci uinput /proc/misc)"
ls -la /dev/uinput
timeout 120 /test --nocapture --test-threads=1
echo "E2E_EXIT=$?"
sync
poweroff -f
EOF
chmod +x "$WORKDIR/rootfs/init"

(cd "$WORKDIR/rootfs" && find . -print0 | cpio --null -o -H newc 2>/dev/null | gzip -9) > "$WORKDIR/initramfs.cpio.gz"

timeout 300 "$QEMU" -machine accel=tcg -cpu max -m 512 -smp 2 \
    -kernel "$WORKDIR/vmlinuz" -initrd "$WORKDIR/initramfs.cpio.gz" \
    -append "console=ttyS0 rdinit=/init panic=-1 loglevel=3" \
    -nographic -no-reboot 2>&1 | tee "$WORKDIR/qemu.log" || true

if grep -q "E2E_EXIT=0" "$WORKDIR/qemu.log" && grep -q "test result: ok" "$WORKDIR/qemu.log"; then
    echo "e2e: OK"
else
    echo "error: e2e failed, full log: $WORKDIR/qemu.log" >&2
    exit 1
fi
