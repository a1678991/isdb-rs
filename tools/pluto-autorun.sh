#!/bin/sh
# isdb-rs: RX-only bench. Stops Tezuka's DATV, GPS, MQTT and NFS services
# and keeps the transmitter powered down. Sourced at boot by
# /etc/init.d/S98autostart; delete this file to restore Tezuka's defaults.
#
# Install (Tezuka on a PlutoSky R2; persistent in the board's jffs2):
#   ssh root@PLUTO 'f=/mnt/jffs2/autorun.sh; cat > $f.new && sh -n $f.new &&
#     { [ ! -e $f ] || cp $f $f.prev; } && mv $f.new $f && chmod 600 $f && sync' \
#     < tools/pluto-autorun.sh
# (written to .new and syntax-checked before it replaces the old hook,
# which is kept as autorun.sh.prev)
# The copy on the bench board is this file without this install note.
isdb_quiet() {
  # avahi-daemon stays: `--radio pluto` without a uri finds the board by
  # mDNS. Watchers and controllers first, so nothing restarts what follows
  # ([x]yz patterns never match the shell running this script)
  for p in '[w]atchdatveasy' '[w]atchconsolefreq' '[w]atchconsoletx' '[a]pi_controller' \
           '[m]qtt_obs_ctrl' '[b]itrate_strategy' '[m]qtt_longmynd' '[l]nb_config'; do
    pkill -f "$p"
  done
  killall -q pluto_mqtt_ctrl pluto_stream zynqrx mqtt_pubd mosquitto_sub gpspipe \
             inotifywait mosquitto gpsd rpc.mountd rpc.statd rpcbind classifier
  for d in /sys/bus/iio/devices/iio:device*; do
    [ "$(cat $d/name)" = ad9361-phy ] || continue
    echo 1 > $d/out_altvoltage1_TX_LO_powerdown
    echo -89.75 > $d/out_voltage0_hardwaregain
    echo -89.75 > $d/out_voltage1_hardwaregain 2>/dev/null
  done
}
isdb_quiet
# S97's start_zynqrx and the DATV scripts start in the background: a second pass
( sleep 20; isdb_quiet ) </dev/null >/dev/null 2>&1 &

# TCP congestion control: cubic instead of the BBR that S96networkcong sets.
# BBR's PROBE_RTT holds the sender to 4 packets for >= 200 ms every 10 s,
# which stalls iiod's symbol stream (isdb-rs VERIFICATION.md "The stall is
# BBR's PROBE_RTT"). New connections only; S96's fq qdisc and send buffers
# stay as they are.
sysctl -qw net.ipv4.tcp_congestion_control=cubic

# Static address back after a link flap: Tezuka's ifplugd.action flushes
# eth0's addresses on link down and only reacquires them by DHCP on link
# up, so a static ipaddr_eth (fw_printenv) is lost until reboot. Re-add it
# whenever the link is up without it. Polls once a second.
isdb_ip=$(fw_printenv -n ipaddr_eth 2>/dev/null)
isdb_mask=$(fw_printenv -n netmask_eth 2>/dev/null)
case "$isdb_ip" in
  ''|dhcp) ;;
  *)
    isdb_prefix=16
    [ "$isdb_mask" = 255.255.255.0 ] && isdb_prefix=24
    (
      while :; do
        if [ "$(cat /sys/class/net/eth0/carrier 2>/dev/null)" = 1 ] &&
           ! ip -4 addr show dev eth0 | grep -q "inet $isdb_ip/"; then
          ip addr add "$isdb_ip/$isdb_prefix" dev eth0 2>/dev/null
          logger -t isdb-rs "eth0: restored $isdb_ip/$isdb_prefix after a link flap"
        fi
        sleep 1
      done
    ) </dev/null >/dev/null 2>&1 &
    ;;
esac
