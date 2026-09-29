#!/bin/sh
# Kobo radio adapter. Interface/driver hints are captured by run.sh from Nickel.
set -eu
step='validate hardware hints'
trap 'status=$?; if [ "$status" -ne 0 ]; then printf "Wi-Fi helper failed at %s (status=%s)\n" "$step" "$status" >&2; fi' 0
case "${INTERFACE:-}" in ''|-*|.*|*[!a-zA-Z0-9_.-]*) printf 'Missing or invalid Wi-Fi interface\n' >&2; exit 2 ;; esac
case "${PLATFORM:-}" in -*|.*|*[!a-zA-Z0-9_.-]*) printf 'Invalid Wi-Fi platform\n' >&2; exit 2 ;; esac
case "${WIFI_MODULE:-}" in dhd|8189fs|8189es|8723bs|8723ds|8821cs|moal) ;; *) printf 'Missing or unsupported Wi-Fi driver: %s\n' "${WIFI_MODULE:-unset}" >&2; exit 2 ;; esac
resolve_module() {
    # PLATFORM is not exported by every Nickel launcher. Shutdown does not
    # need it; startup can locate a unique installed driver instead.
    module_file=''
    if [ -n "${PLATFORM:-}" ]; then
        for candidate in "/drivers/$PLATFORM/$WIFI_MODULE.ko" "/drivers/$PLATFORM/wifi/$WIFI_MODULE.ko"; do
            if [ -f "$candidate" ]; then module_file=$candidate; break; fi
        done
    else
        for candidate in /drivers/*/"$WIFI_MODULE.ko" /drivers/*/wifi/"$WIFI_MODULE.ko"; do
            [ -f "$candidate" ] || continue
            [ -z "$module_file" ] || { printf 'Multiple Wi-Fi driver paths; PLATFORM is required\n' >&2; return 3; }
            module_file=$candidate
        done
    fi
    [ -n "$module_file" ] || { printf 'Wi-Fi driver file is missing\n' >&2; return 3; }
    module_dir=$(dirname "$module_file")
    [ ! -d "$module_dir/wifi" ] || module_dir="$module_dir/wifi"
}
loaded() { awk -v module="$1" '$1 == module { found=1 } END { exit !found }' /proc/modules; }
remove_module() {
    # Supplicant termination is asynchronous; the driver can remain busy briefly.
    remaining=20
    while loaded "$1"; do
        if rmmod "$1"; then return 0; fi
        remaining=$((remaining-1))
        [ "$remaining" -gt 0 ] || return 1
        sleep 0.2
    done
}
ntx_power() {
    # Use the app's libc adapter instead of depending on a firmware ioctl utility.
    "$(dirname "$0")/desktop-gpui-kobo" --wifi-power-ioctl "$1"
}
case "${1:-}" in
    on)
        step="start radio"
        if ! loaded "$WIFI_MODULE"; then
            resolve_module
            if [ "$WIFI_MODULE" = moal ]; then
                ntx_power 1
            else
                loaded sdio_wifi_pwr || insmod "$module_dir/sdio_wifi_pwr.ko"
            fi
            country=$(sed -n 's/^WifiRegulatoryDomain=//p' '/mnt/onboard/.kobo/Kobo/Kobo eReader.conf' 2>/dev/null | head -n 1)
            case "$country" in [A-Z][A-Z]) ;; *) country='' ;; esac
            if [ "$WIFI_MODULE" = moal ]; then
                dep="$module_dir/mlan.ko"
                [ -f "$dep" ] || dep="$(dirname "$module_dir")/mlan.ko"
                loaded mlan || insmod "$dep"
                if [ -n "$country" ]; then
                    insmod "$module_file" mod_para=nxp/wifi_mod_para_sd8987.conf "reg_alpha2=$country"
                else
                    insmod "$module_file" mod_para=nxp/wifi_mod_para_sd8987.conf
                fi
            elif [ "$WIFI_MODULE" = 8821cs ] && [ -n "$country" ]; then
                insmod "$module_file" "rtw_country_code=$country"
            else
                insmod "$module_file"
            fi
        fi
        remaining=20
        while [ ! -d "/sys/class/net/$INTERFACE" ]; do
            [ "$remaining" -gt 0 ] || exit 4
            remaining=$((remaining-1))
            sleep 0.2
        done
        ifconfig "$INTERFACE" up
        [ "$WIFI_MODULE" != dhd ] || wlarm_le -i "$INTERFACE" up
        if ! wpa_cli -i "$INTERFACE" ping 2>/dev/null | grep -q '^PONG$'; then
            driver=wext
            [ "$WIFI_MODULE" != moal ] || driver=nl80211
            env -u LD_LIBRARY_PATH wpa_supplicant -B -D "$driver" -i "$INTERFACE" \
                -c /etc/wpa_supplicant/wpa_supplicant.conf -C /var/run/wpa_supplicant
        fi
        ;;
    off)
        step="stop supplicant and bring $INTERFACE down"
        if [ -d "/sys/class/net/$INTERFACE" ]; then
            wpa_cli -i "$INTERFACE" terminate >/dev/null 2>&1 || true
            [ "$WIFI_MODULE" != dhd ] || wlarm_le -i "$INTERFACE" down
            ifconfig "$INTERFACE" down
        fi
        step="unload $WIFI_MODULE"
        remove_module "$WIFI_MODULE"
        if [ "$WIFI_MODULE" = moal ]; then
            step="unload mlan"
            remove_module mlan
        fi
        step="switch radio power off"
        # Match Plato's driver-specific power method. An already unloaded
        # power module is already off; it must not trigger an unrelated ioctl.
        if [ "$WIFI_MODULE" = moal ]; then ntx_power 0; else remove_module sdio_wifi_pwr; fi
        ;;
    *) exit 2 ;;
esac
