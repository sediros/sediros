# SedirOS: on the console, tell a user who has no SSH key yet how to add one.
# Shown only after login, only to that user and only about their own
# ~/.ssh/authorized_keys; it disappears once a key is there. Not in the
# live session (nothing is kept there) and not over SSH (a key is in use).
if [ -t 1 ] && [ -z "${SSH_CONNECTION:-}" ] && [ "$(id -u)" -ne 0 ] \
    && [ ! -d /run/live/rootfs ] \
    && id -nG 2>/dev/null | tr ' ' '\n' | grep -qxE 'sudo|sedir-ssh' \
    && ! grep -qsE '^(ssh-|ecdsa-|sk-)' "${HOME:-/nonexistent}/.ssh/authorized_keys"; then
    printf '\n  No SSH key yet: SSH accepts keys only. To add yours:\n'
    printf '      sudo sedir-ssh-enroll\n\n'
fi
