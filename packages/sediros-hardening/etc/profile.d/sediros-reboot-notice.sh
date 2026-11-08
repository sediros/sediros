# SedirOS: tell interactive login shells when an update needs a reboot.
# /run/reboot-required is written by the kernel postinst hooks.
if [ -f /run/reboot-required ] && [ -t 1 ]; then
    printf '\n*** System restart required'
    if [ -s /run/reboot-required.pkgs ]; then
        printf ' (%s)' "$(sort -u /run/reboot-required.pkgs | tr '\n' ' ' | sed 's/ $//')"
    fi
    printf ' ***\n\n'
fi
