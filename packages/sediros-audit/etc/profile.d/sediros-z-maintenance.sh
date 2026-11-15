# SedirOS: after the "System restart required" notice
# (sediros-reboot-notice.sh), say what the maintenance window will do.
if [ -f /run/reboot-required ] && [ -t 1 ] && [ -r /etc/sediros/maintenance.conf ]; then
    _sediros_mode="$(sed -n 's/^[[:space:]]*MODE=["]*\([a-z]*\).*/\1/p' /etc/sediros/maintenance.conf | tail -n 1)"
    case "$_sediros_mode" in
        ask)
            if [ "$(head -n 1 /var/lib/sediros/maintenance/approved 2>/dev/null)" != "$(cat /proc/sys/kernel/random/boot_id)" ]; then
                printf 'Reboot in the next maintenance window: sudo sediros-maintenance approve\n\n'
            fi ;;
        auto)
            printf 'It reboots in the next maintenance window: sediros-maintenance status\n\n' ;;
    esac
    unset _sediros_mode
fi
