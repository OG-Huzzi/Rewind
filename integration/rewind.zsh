# RewindUndo passive shell observation for zsh.
#
# Usage: add the following line to ~/.zshrc (adjust the path):
#     source /path/to/rewind/integration/rewind.zsh
#
# The integration manages the session id for you; you never need to invent
# or inject REWIND_SESSION_ID manually. Every hook invocation fails open:
# if `rewind` is missing, not initialized for the current directory, or
# errors, the shell command runs exactly as it would have without Rewind,
# and no hook failure can change the command's exit status.
#
# Boundary identity contract (Phase 1.4): the pre-hook creates the durable
# boundary and prints its immutable id on stdout. The shell keeps that id
# for exactly the command whose post-hook will follow, and hands it back to
# the background post-hook. A post-hook never looks for "the newest
# boundary": several background hooks can be in flight at once, so recency,
# timestamps, command text, cwd, or session ordering can never identify a
# boundary.
#
# Responsiveness contract (Phase 1.3): the pre-hook is deliberately
# synchronous but extremely lightweight (a single boundary INSERT). The
# post-hook's bookkeeping — scan, CAS, persistence — is launched in the
# BACKGROUND with nohup (which also survives zsh's default HUP-on-exit for
# jobs), so the interactive shell returns to the prompt immediately and
# never waits for Rewind's expensive work. If the background bookkeeping
# cannot complete, it degrades conservatively (bypass marker /
# CAPTURE_FAILED -> unknown interval -> RECONCILIATION_REQUIRED on the next
# writer); it never fabricates an operation and never advances a trusted
# baseline from incomplete information.

# One stable session id per interactive shell session. It is recorded with
# every boundary for provenance only — it is never used to correlate a
# post-hook with a boundary. It is materialized once, in the shell itself
# (see the registration block at the end): assigning it inside a command
# substitution would only set it in the subshell.
_rewind_session_id() {
    if [ -z "${REWIND_SESSION_ID:-}" ]; then
        REWIND_SESSION_ID="zsh-$$-$(date +%s 2>/dev/null || echo 0)"
        export REWIND_SESSION_ID
    fi
    printf '%s' "$REWIND_SESSION_ID"
}

# Pre-command boundary; $1 is the full command line about to execute. The
# pre-hook prints ONLY its immutable boundary id on stdout, which is kept in
# _REWIND_BOUNDARY_ID for this command's post-hook; diagnostics go to stderr
# and are discarded unless REWIND_HOOK_VERBOSE is set. zsh restores $? after
# preexec, so this cannot change the status the next command observes.
_rewind_preexec() {
    if command -v rewind >/dev/null 2>&1; then
        if [ -n "${REWIND_HOOK_VERBOSE:-}" ]; then
            _REWIND_BOUNDARY_ID=$(rewind hook pre --command "$1" --session "$(_rewind_session_id)")
        else
            _REWIND_BOUNDARY_ID=$(rewind hook pre --command "$1" --session "$(_rewind_session_id)" 2>/dev/null)
        fi
    else
        _REWIND_BOUNDARY_ID=""
    fi
    return 0
}

# Post-command boundary; $? still holds the finished command's exit status.
# Bookkeeping is spawned in the background: the shell returns to the prompt
# without waiting for it. The id is consumed (cleared) here exactly once, so
# it can never leak into a later command line.
_rewind_precmd() {
    local _rewind_status=$?
    local _rewind_boundary="${_REWIND_BOUNDARY_ID:-}"
    _REWIND_BOUNDARY_ID=""
    if [ -n "$_rewind_boundary" ] && command -v rewind >/dev/null 2>&1; then
        nohup rewind hook post \
            --boundary "$_rewind_boundary" \
            --exit-code "$_rewind_status" \
            </dev/null >/dev/null 2>&1 &
    fi
    return 0
}

# add-zsh-hook must be autoloaded BEFORE it can be detected: an
# autoloadable function is not yet visible to `command -v` / $functions.
autoload -Uz add-zsh-hook 2>/dev/null
if (( $+functions[add-zsh-hook] )); then
    _rewind_session_id >/dev/null
    add-zsh-hook preexec _rewind_preexec
    add-zsh-hook precmd _rewind_precmd
fi
