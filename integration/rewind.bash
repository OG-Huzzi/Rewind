# RewindUndo passive shell observation for bash.
#
# Usage: add the following line to ~/.bashrc (adjust the path):
#     source /path/to/rewind/integration/rewind.bash
#
# The integration manages the session id for you; you never need to invent
# or inject REWIND_SESSION_ID manually. Every hook invocation fails open:
# if `rewind` is missing, not initialized for the current directory, or
# errors, the shell command runs exactly as it would have without Rewind,
# and no hook failure can change the command's exit status.
#
# Responsiveness contract (Phase 1.3): the pre-hook is deliberately
# synchronous but extremely lightweight (a single boundary INSERT). The
# post-hook's bookkeeping — scan, CAS, persistence — is launched in the
# BACKGROUND with nohup, so the interactive shell returns to the prompt
# immediately and never waits for Rewind's expensive work. If the
# background bookkeeping cannot complete, it degrades conservatively
# (bypass marker / CAPTURE_FAILED -> unknown interval ->
# RECONCILIATION_REQUIRED on the next writer); it never fabricates an
# operation and never advances a trusted baseline from incomplete
# information.

# One stable session id per interactive shell session.
_rewind_session_id() {
    if [ -z "${REWIND_SESSION_ID:-}" ]; then
        REWIND_SESSION_ID="bash-$$-$(date +%s 2>/dev/null || echo 0)"
        export REWIND_SESSION_ID
    fi
    printf '%s' "$REWIND_SESSION_ID"
}

# Never let a synchronous hook failure reach the shell (pre-hook only).
# Set REWIND_HOOK_VERBOSE=1 to see hook diagnostics while configuring.
_rewind_hook() {
    command -v rewind >/dev/null 2>&1 || return 0
    if [ -n "${REWIND_HOOK_VERBOSE:-}" ]; then
        "$@" || return 0
    else
        "$@" >/dev/null 2>&1 || return 0
    fi
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
# $? still holds its exit status). Bookkeeping is spawned in the background:
# the shell returns to the prompt without waiting for it.
_rewind_precmd() {
    local _rewind_status=$?
    if [ -n "${_REWIND_LAST_COMMAND:-}" ]; then
        if command -v rewind >/dev/null 2>&1; then
            nohup rewind hook post \
                --exit-code "$_rewind_status" \
                --session "$(_rewind_session_id)" \
                </dev/null >/dev/null 2>&1 &
            disown 2>/dev/null || true
        fi
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
