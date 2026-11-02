#!/usr/bin/env bash
# Bitstream selection on the bench PlutoSky R2 (SD boot, Tezuka): what it
# boots, putting a new image beside the others, rolling back, and the
# ad9361 rebind recovery (docs/plans/isdb-s.md §2). Never flashes; the only
# files it writes on the card are the new .bin, uEnv.txt and the saved
# uEnv.txt copy. Nothing here transmits.
#
#   tools/pluto-image.sh status                 uEnv.txt, the .bin files, IIO devices, ad9361 tune messages
#   tools/pluto-image.sh deploy FILE.bin SAVE   copy FILE.bin to /boot (refuses an existing name), save
#                                               uEnv.txt as /boot/SAVE (refuses an existing one), set
#                                               bitstream_image=FILE.bin, sync (no reboot)
#   tools/pluto-image.sh rollback SAVE          restore /boot/uEnv.txt from /boot/SAVE, sync (no reboot)
#   tools/pluto-image.sh reboot                 sync; reboot, then wait for ssh (up to 180 s)
#   tools/pluto-image.sh rebind                 the §2 recovery when cf-ad9361-lpc is missing
#   tools/pluto-image.sh run 'COMMAND'          one command on the board
#
# HOST (default 10.10.0.30), SSHPASS (default analog: Tezuka's root).
set -euo pipefail
HOST=${HOST:-10.10.0.30}
export SSHPASS=${SSHPASS:-analog}
SSH=(sshpass -e ssh -o UserKnownHostsFile=/dev/null -o StrictHostKeyChecking=no -o LogLevel=ERROR
     -o ConnectTimeout=5 "root@$HOST")
SCP=(sshpass -e scp -O -o UserKnownHostsFile=/dev/null -o StrictHostKeyChecking=no -o LogLevel=ERROR)
# the images that must never be overwritten
PROTECTED="isdb_txrx2_top.bin isdb_txrx_top.bin isdb_top.bin system_top.bin tezuka_top.bin"

on() { "${SSH[@]}" "$@"; }

case "${1:-}" in
status)
    on 'echo "== uEnv.txt"; grep -n "^bitstream_image" /boot/uEnv.txt; echo "== /boot"; ls -la /boot/*.bin /boot/uEnv.txt*; \
        echo "== IIO"; for d in /sys/bus/iio/devices/iio:device*; do echo "$d $(cat $d/name)"; done; \
        echo "== dmesg (tune)"; dmesg | grep -iE "tuning|ad9361|cf_axi" | tail -8; uptime'
    ;;
deploy)
    bin=${2:?deploy FILE.bin SAVE}
    save=${3:?deploy FILE.bin SAVE}
    name=$(basename "$bin")
    for p in $PROTECTED; do [ "$name" != "$p" ] || { echo "refusing to overwrite $p" >&2; exit 2; }; done
    on "test ! -e /boot/$name" || { echo "/boot/$name exists: refusing" >&2; exit 2; }
    on "test ! -e /boot/$save" || { echo "/boot/$save exists: refusing" >&2; exit 2; }
    "${SCP[@]}" "$bin" "root@$HOST:/boot/$name.tmp"
    local_sum=$(sha256sum "$bin" | cut -d' ' -f1)
    remote_sum=$(on "sha256sum /boot/$name.tmp" | cut -d' ' -f1)
    [ "$local_sum" = "$remote_sum" ] || { echo "sha256 mismatch after copy" >&2; on "rm -f /boot/$name.tmp"; exit 1; }
    on "mv /boot/$name.tmp /boot/$name && cp /boot/uEnv.txt /boot/$save && \
        sed -i 's/^bitstream_image=.*/bitstream_image=$name/' /boot/uEnv.txt && sync && \
        grep -n bitstream_image /boot/uEnv.txt /boot/$save"
    echo "deployed $name (sha256 $local_sum); previous uEnv.txt in /boot/$save"
    ;;
rollback)
    save=${2:?rollback SAVE}
    on "test -f /boot/$save && cp /boot/$save /boot/uEnv.txt && sync && grep -n bitstream_image /boot/uEnv.txt"
    ;;
reboot)
    # a background reboot inside the ssh command dies with the session: run it directly
    timeout 10 "${SSH[@]}" 'sync; reboot' || true
    sleep 20
    for _ in $(seq 1 32); do
        if on true 2>/dev/null; then echo "ssh back"; exit 0; fi
        sleep 5
    done
    echo "no ssh after 180 s" >&2
    exit 1
    ;;
rebind)
    on 'D=/sys/bus/platform/drivers; \
        echo 79024000.cf-ad9361-dds-core-lpc > $D/cf_axi_dds/unbind 2>/dev/null; \
        echo spi0.0 > /sys/bus/spi/drivers/ad9361/unbind 2>/dev/null; \
        echo spi0.0 > /sys/bus/spi/drivers/ad9361/bind; \
        echo 79020000.cf-ad9361-lpc > $D/cf_axi_adc/bind; \
        /etc/init.d/S23udc restart; \
        for d in /sys/bus/iio/devices/iio:device*; do echo "$d $(cat $d/name)"; done'
    ;;
run)
    on "${2:?run COMMAND}"
    ;;
*)
    sed -n '2,20p' "$0"
    exit 2
    ;;
esac
