#!/bin/sh
# Read-only inventory. Run on the Kobo before and during Bokheim.
# Does not scan, connect, change radio power, or read saved credentials.

printf 'Kobo Wi-Fi inventory\n'
uname -srmo

printf '\nLauncher hardware hints\n'
printf 'PRODUCT=%s\nPLATFORM=%s\nINTERFACE=%s\nWIFI_MODULE=%s\n' \
    "${PRODUCT:-unknown}" "${PLATFORM:-unknown}" "${INTERFACE:-unknown}" "${WIFI_MODULE:-unknown}"
nickel_pid=$(pidof nickel 2>/dev/null | awk '{print $1}')
if [ -n "$nickel_pid" ] && [ -r "/proc/$nickel_pid/environ" ]; then
    printf 'Selected Nickel environment entries:\n'
    tr '\000' '\n' < "/proc/$nickel_pid/environ" |
        grep -E '^(PRODUCT|PLATFORM|INTERFACE|WIFI_MODULE|WIFI_MODULE_PATH)='
fi

printf '\nAvailable utilities\n'
for utility in wpa_supplicant wpa_cli udhcpc dhcpcd ip ifconfig iw iwconfig insmod rmmod timeout; do
    command -v "$utility" 2>/dev/null || true
done
if command -v wpa_supplicant >/dev/null 2>&1; then
    wpa_supplicant -v 2>/dev/null
fi

printf '\nRunning network services (PIDs only)\n'
for service in nickel wpa_supplicant udhcpc dhcpcd dhcpcd-dbus connmand NetworkManager; do
    service_pids=$(pidof "$service" 2>/dev/null)
    printf '%s: %s\n' "$service" "${service_pids:-not running}"
done

printf '\nNetwork interface drivers and state\n'
for interface_path in /sys/class/net/*; do
    [ -d "$interface_path" ] || continue
    interface_name=${interface_path##*/}
    [ "$interface_name" = lo ] && continue
    printf '%s\n' "$interface_name"
    for attribute in operstate carrier; do
        if [ -r "$interface_path/$attribute" ]; then
            printf '  %s: ' "$attribute"
            cat "$interface_path/$attribute" 2>/dev/null || true
        fi
    done
    if [ -e "$interface_path/device/driver/module" ]; then
        printf '  module: '
        readlink "$interface_path/device/driver/module"
    fi
done

printf '\nKnown radio modules currently loaded\n'
awk '$1 ~ /^(sdio_wifi_pwr|dhd|brcmfmac|8189fs|8189es|8723bs|8821cs|moal|mlan|wlan_drv_gen4m|wmt_drv|wmt_chrdev_wifi)$/ {print $1}' /proc/modules

printf '\nSupplicant control sockets\n'
for socket_dir in /var/run/wpa_supplicant /run/wpa_supplicant; do
    [ -d "$socket_dir" ] || continue
    for control_socket in "$socket_dir"/*; do
        [ -S "$control_socket" ] || continue
        printf '%s\n' "$control_socket"
        if command -v timeout >/dev/null 2>&1 && command -v wpa_cli >/dev/null 2>&1; then
            timeout 3 wpa_cli -p "$socket_dir" -i "${control_socket##*/}" ping 2>&1 || true
        fi
    done
done

printf '\nFirmware hooks present\n'
for firmware_path in /etc/udhcpc.d/default.script /etc/wpa_supplicant/wpa_supplicant.conf /dev/ntx_io /dev/wmtWifi; do
    [ ! -e "$firmware_path" ] || printf '%s\n' "$firmware_path"
done
printf '\nInventory complete\n'
