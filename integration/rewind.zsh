# RewindUndo passive shell observation for zsh.
#
# Usage: add the following line to ~/.zshrc (adjust the path):
#     source /path/to/rewind/integration/rewind.zsh
#
# The integration manages the session id for you; you never need to invent
# or inject REWIND_SESSION_ID manually. Every hook invocation fails open:
# if `rewind` is missing, not initialized for the current directory, or
# errors, the shell command runs exactly as it would have without Rewind.

# One stable session id per interactive shell session.
_rewind_session_id() {
    if [ -z "${REWIND_SESSION_ID:-}" ]; then
        REWIND_SESSION_ID="zsh-$$-$(date +%s 2>/dev/null || echo 0)"
        export REWIND_SESSION_ID
    fi
    printf '%s' "$REWIND_SESSION_ID"
}

# Never let a hook failure reach the shell.
_rewind_hook() {
    command -v rewind >/dev/null 2>&1 || return 0
    "$@" >/dev/null 2>&1 || return 0
}

# Pre-command boundary; $1 is the full command line about to execute.
_rewind_preexec() {
    _REWIND_LAST_COMMAND="$1"
    _rewind_hook rewind hook pre \
        --command "$1" \
        --session "$(_rewind_session_id)"
    return 0
}

# Post-command boundary; $? still holds the finished command's exit status.
_rewind_precmd() {
    local _rewind_status=$?
    if [ -n "${_REWIND_LAST_COMMAND:-}" ]; then
        _rewind_hook rewind hook post \
            --exit-code "$_rewind_status" \
            --session "$(_rewind_session_id)"
        _REWIND_LAST_COMMAND=""
    fi
    return 0
}

if command -v add-zsh-hook >/dev/null 2>&1; then
    autoload -Uz add-zsh-hook
    add-zsh-hook preexec _rewind_preexec
    add-zsh-hook precmd _rewind_precmd
fi
