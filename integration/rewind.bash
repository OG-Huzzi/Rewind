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
# BACKGROUND with nohup, so the interactive shell returns to the prompt
# immediately and never waits for Rewind's expensive work. If the
# background bookkeeping cannot complete, it degrades conservatively
# (bypass marker / CAPTURE_FAILED -> unknown interval ->
# RECONCILIATION_REQUIRED on the next writer); it never fabricates an
# operation and never advances a trusted baseline from incomplete
# information.

# One stable session id per interactive shell session. It is recorded with
# every boundary for provenance only — it is never used to correlate a
# post-hook with a boundary. It is materialized once, in the shell itself
# (see the interactive block at the end): assigning it inside a command
# substitution would only set it in the subshell.
_rewind_session_id() {
    if [ -z "${REWIND_SESSION_ID:-}" ]; then
        REWIND_SESSION_ID="bash-$$-$(date +%s 2>/dev/null || echo 0)"
        export REWIND_SESSION_ID
    fi
    printf '%s' "$REWIND_SESSION_ID"
}

# Creates the pre-command boundary and prints ONLY its immutable id on
# stdout, so the caller can capture it for the matching post-hook. Hook
# diagnostics go to stderr and can never contaminate the captured value;
# they are discarded unless REWIND_HOOK_VERBOSE is set.
_rewind_pre_boundary() {
    command -v rewind >/dev/null 2>&1 || return 0
    if [ -n "${REWIND_HOOK_VERBOSE:-}" ]; then
        rewind hook pre --command "$1" --session "$(_rewind_session_id)"
    else
        rewind hook pre --command "$1" --session "$(_rewind_session_id)" 2>/dev/null
    fi
}

# Pre-command boundary (DEBUG trap fires for every simple command). The
# captured id belongs to this command; the post-hook will be given exactly
# this id. Bash restores $? after the DEBUG trap, so this cannot change the
# status the next command observes.
_rewind_preexec() {
    [ -n "$BASH_COMMAND" ] || return 0
    case "$BASH_COMMAND" in
        _rewind_*|rewind_session_id|*PROMPT_COMMAND*|trap\ *) return 0 ;;
    esac
    _REWIND_BOUNDARY_ID=$(_rewind_pre_boundary "$BASH_COMMAND")
    return 0
}

# Post-command boundary (PROMPT_COMMAND runs after the command completed and
# $? still holds its exit status). Bookkeeping is spawned in the background:
# the shell returns to the prompt without waiting for it. The id is consumed
# (cleared) here exactly once, so it can never leak into a later command.
_rewind_precmd() {
    local _rewind_status=$?
    local _rewind_boundary="${_REWIND_BOUNDARY_ID:-}"
    _REWIND_BOUNDARY_ID=""
    if [ -n "$_rewind_boundary" ] && command -v rewind >/dev/null 2>&1; then
        nohup rewind hook post \
            --boundary "$_rewind_boundary" \
            --exit-code "$_rewind_status" \
            </dev/null >/dev/null 2>&1 &
        disown 2>/dev/null || true
    fi
    return 0
}

case "$-" in
    *i*)
        _rewind_session_id >/dev/null
        # Update this before installing the DEBUG trap below: with the trap
        # active, the DEBUG handler would otherwise see these very loading
        # commands and record a spurious boundary for them. The trap
        # installation must be the last thing this file does, so that sourcing
        # the integration never fabricates a boundary for its own code.
        case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in
            "declare -a"*)
                PROMPT_COMMAND=("_rewind_precmd" ${PROMPT_COMMAND[@]+"${PROMPT_COMMAND[@]}"})
                ;;
            *)
                PROMPT_COMMAND="_rewind_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
                ;;
        esac
        trap '_rewind_preexec' DEBUG
        ;;
esac
