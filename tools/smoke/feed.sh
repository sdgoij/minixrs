#!/bin/sh
# Drive the userspace smoke scenario on a booted image.
#
#   sh tools/smoke/feed.sh <log> <seconds> <qemu command…>
#
# The command is the arch's QEMU line, whose serial console is stdio. This types `scenario.tsv` into
# the shell a user would get, one line at a time, waits for each line's answer, then stops the guest.
# The log holds everything the guest printed, so the caller's `_assert-qemu-log` still works on it.
#
# Why the guest's stdin is a pipe and not a FIFO — measured, and the reason this file is shaped the
# way it is: a FIFO is the obvious way to hold one end open while the other is the guest's stdin, but
# `mkfifo` under git-bash is emulated, and the QEMU there is a native Windows binary (`PE32+
# executable`), which reads nothing from one at all. Measured: with a FIFO on stdin the guest printed
# `wserver: ready` and its prompt and then never saw a byte, while the same bytes through a pipe were
# echoed and executed. So the steps go out on the left side of a pipe — which makes that side a
# subshell, so its verdict comes back through a one-line status file rather than an exit status.
#
# Why paced, and why it waits for the prompt: input written to the console before the shell is reading
# it is *dropped*, not buffered. Measured on the shipped x86 image — `printf '/bin/echo alive\n' |
# qemu …` printed nothing, and the same bytes after a delay printed the command, its output and the
# next prompt. That was Step 2's open question in TEST_GATES.md, and this is the answer it recorded:
# the feeder types after the prompt, in order, one step at a time.
#
# Fails on the step that did not answer, naming it: a scenario that stops should say which of its
# steps stopped the system, not that `wserver: ready` is missing.
#
# POSIX sh, so it runs under the Justfile's shell on Windows's git-bash too.

set -e

log=$1
shift
seconds=$1
shift

here=$(cd "$(dirname "$0")" && pwd)
scenario="$here/scenario.tsv"
steps_file="$log.steps"
status="$log.status"
tab=$(printf '\t')

poll=0.2
# The driver stops a second before the guest's own timeout, so a step that hangs is always reported by
# the driver - which can name it - and never as the guest having gone quiet for another reason.
budget=$((seconds * 5 - 5))
# The caller's backstop: it has to outlast the driver, or the precise verdict loses the race and a
# stopped scenario reports as `never reported`.
backstop=$((budget + 25))

rm -f "$log" "$steps_file" "$status"
# `tr -d` rather than a `\r`-stripping expansion: this runs under whatever `sh` the Justfile found, and
# a scenario file written on Windows would otherwise hand every step a trailing carriage return.
tr -d '\r' < "$scenario" > "$steps_file"

# Wait until `pattern` appears in the log as it accumulates, or the budget runs out. `pattern` is an
# extended regular expression, anchored by the caller. The budget is spent across all of the driver's
# waits rather than per wait, so the whole scenario is bounded by the guest's own lifetime. The log may
# not exist yet on the first poll, which is the only reason the error is suppressed.
waited=0
wait_for() {
    pattern=$1
    while [ "$waited" -lt "$budget" ]; do
        if grep -qE -- "$pattern" "$log" 2>/dev/null; then
            return 0
        fi
        sleep "$poll"
        waited=$((waited + 1))
    done
    return 1
}

# `<verdict> <tab> <why>`, or `ok` and what ran. The driver below is a subshell and this is its only
# way out; `$status` not existing at all means it never got to say anything.
write_status() {
    printf '%s\t%s\n' "$1" "$2" > "$status"
}

# The reading end of the pipe into the guest. stdout is the guest's console, so every word meant for a
# human goes to stderr.
drive() {
    if ! wait_for '^#[[:space:]]*$'; then
        write_status "no prompt" "the guest never reached a shell"
        return 1
    fi

    steps=0
    # `IFS=` so the line arrives verbatim: the default IFS would eat the tab that ends a step whose
    # expectation is empty, which is every write.
    while IFS= read -r line; do
        case "$line" in
            '' | '#'*) continue ;;
        esac
        steps=$((steps + 1))
        case "$line" in
            *"$tab"*) ;;
            *)
                # A space in place of the tab would fold the expectation into the command, and the
                # step would then pass on its own echo — the failure this file exists to catch.
                write_status "step $steps" "has no tab separator: '$line'"
                return 1
                ;;
        esac
        send=${line%%"$tab"*}
        expect=${line#*"$tab"}

        printf '%s\n' "$send"
        if [ -n "$expect" ]; then
            # The whole line, not a substring: the guest echoes what it is sent, and for the first step
            # that echoed text contains the expectation too (TEST_GATES.md cause (h)).
            if ! wait_for "^${expect}[[:space:]]*$"; then
                write_status "step $steps" "did not answer: sent '$send', expected the line '$expect'"
                return 1
            fi
        elif ! wait_for "^#[[:space:]]*${send}[[:space:]]*$"; then
            # Nothing to read back, so the echoed command is the whole of the step's evidence.
            write_status "step $steps" "was not accepted: sent '$send'"
            return 1
        fi
        echo "  ok  $send" >&2
        # A step that answered is not yet proof the next one will be read: the shell may not be back at
        # its prompt, and input sent before then is dropped.
        if ! wait_for "^#[[:space:]]*$"; then
            write_status "step $steps" "answered, but the shell did not come back to a prompt"
            return 1
        fi
    done < "$steps_file"

    if [ "$steps" -eq 0 ]; then
        write_status "no steps" "there are none in $scenario"
        return 1
    fi
    write_status ok "$steps steps"
    return 0
}

drive | /usr/bin/timeout -s 9 "$seconds" "$@" > "$log" 2>&1 &
guest=$!

cleanup() {
    kill "$guest" 2>/dev/null || true
    wait "$guest" 2>/dev/null || true
    rm -f "$steps_file"
}
trap cleanup EXIT INT TERM

# Wait for the verdict: the driver reports well before this backstop, so this ends as soon as the
# scenario has been driven, whether it passed or stopped.
waited=0
while [ ! -f "$status" ] && [ "$waited" -lt "$backstop" ]; do
    sleep "$poll"
    waited=$((waited + 1))
done

if [ ! -f "$status" ]; then
    echo "!! the smoke scenario never reported — the guest died, or ${seconds}s ran out before its" >&2
    echo "!! first step. Tail of $log:" >&2
    tail -20 "$log" >&2
    exit 1
fi

verdict=$(cut -f1 "$status")
detail=$(cut -f2 "$status")
if [ "$verdict" != ok ]; then
    echo "!! $verdict: $detail. Tail of $log:" >&2
    tail -20 "$log" >&2
    exit 1
fi

echo "the smoke scenario ran ($detail)"
