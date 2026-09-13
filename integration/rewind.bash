# RewindUndo passive shell observation for bash.
#
# Usage: add the following line to ~/.bashrc (adjust the path):
#     source /path/to/rewind/integration/rewind.bash
#
# The integration manages the session id for you; you never need to invent
# or inject REWIND_SESSION_ID manually. Every hook invocation fails open:
# if `rewind` is missing, not initialized for the current directory, or
# errors, the shell command runs exactly as it would have without Rewind.

# One stable session id per interactive shell session.
_rewind_session_id() {
    if [ -z "${REWIND_SESSION_ID:-}" ]; then
        REWIND_SESSION_ID="bash-$$-$(date +%s 2>/dev/null || echo 0)"
        export REWIND_SESSION_ID
    fi
    printf '%s' "$REWIND_SESSION_ID"
}

# Never let a hook failure reach the shell.
_rewind_hook() {
    command -v rewind >/dev/null 2>&1 || return 0
    "$@" >/dev/null 2>&1 || return 0
}

# Pre-command boundary (DEBUG trap fires for every simple command).
_rewind_preexec() {
    [ -n "$BASH_COMMAND" ] || return 0
    case "$BASH_COMMAND" in
        _rewind_*|rewind_session_id|PROMPT_COMMAND*) return 0 ;;
    esac
    _REWIND_LAST_COMMAND="$BASH_COMMAND"
    _rewind_hook rewind hook pre \
        --command "$BASH_COMMAND" \
        --session "$(_rewind_session_id)"
    return 0
}

# Post-command boundary (PROMPT_COMMAND runs after the command completed and
# $? still holds its exit status).
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

case "$-" in
    *i*)
        trap '_rewind_preexec' DEBUG
        PROMPT_COMMAND="_rewind_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
        ;;
esac
