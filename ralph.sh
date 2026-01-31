#!/usr/bin/env bash
set -euo pipefail

# Ralph Loop for blob_share
# Iterates through PLAN.md tasks until all are marked [x]

PLAN="PLAN.md"
MAX_ITERATIONS=50
ITERATION=0

prompt() {
cat <<'PROMPT'
study CLAUDE.md
study PLAN.md and pick the most important thing to do

IMPORTANT:
- author unit tests for the change (or integration tests if more appropriate)
- after making the changes to the files run the tests with `SQLX_OFFLINE=true cargo test --lib`
- run `cargo fmt` and `SQLX_OFFLINE=true cargo clippy -- --deny warnings` and fix any issues
- when tests pass, commit and push the changes
- update PLAN.md when the task is done by changing `### [ ]` to `### [x]` for that task
PROMPT
}

remaining() {
    grep -c '### \[ \]' "$PLAN" 2>/dev/null || echo 0
}

echo "=== Ralph Loop ==="
echo "Plan: $PLAN"
echo "Remaining tasks: $(remaining)"
echo ""

while true; do
    ITERATION=$((ITERATION + 1))
    LEFT=$(remaining)

    if [ "$LEFT" -eq 0 ]; then
        echo ""
        echo "=== ALL TASKS COMPLETE ==="
        echo "Finished after $ITERATION iterations"
        exit 0
    fi

    if [ "$ITERATION" -gt "$MAX_ITERATIONS" ]; then
        echo ""
        echo "=== MAX ITERATIONS ($MAX_ITERATIONS) REACHED ==="
        echo "Remaining tasks: $LEFT"
        exit 1
    fi

    echo "--- Iteration $ITERATION | Remaining: $LEFT ---"

    claude --dangerously-skip-permissions -p "$(prompt)"

    echo ""
    echo "--- Iteration $ITERATION complete ---"
    echo ""
done
