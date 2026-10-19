#!/usr/bin/env bash
#
# NFR-ARCH-1: dependencies point toward core only.
#
# - norte-core never pulls in HTTP, database, vendor or concrete-tool code.
# - Adapters never depend on each other.
#
# The check reads the normal (non-dev, non-build) dependency tree, so crates
# that are only dev-dependencies of a conformance suite do not count.
set -euo pipefail

adapters="norte-anthropic norte-openai norte-gemini norte-tools norte-media norte-testing norte-mcp"

core_forbidden='^(norte-http|norte-(anthropic|openai|gemini|tools|media|testing|mcp)) v|^(reqwest|hyper|axum|tower-http|sqlx|diesel|mongodb|redis|rusqlite|aws-sdk|tokio-postgres) '

status=0

core_tree="$(cargo tree -p norte-core -e normal --prefix none)"
if printf '%s\n' "$core_tree" | grep -Eq "$core_forbidden"; then
    echo "FAIL: norte-core depends on non-core code:" >&2
    printf '%s\n' "$core_tree" | grep -E "$core_forbidden" >&2
    status=1
fi

for crate in $adapters; do
    tree="$(cargo tree -p "$crate" -e normal --prefix none)"
    for other in $adapters; do
        if [ "$crate" = "$other" ]; then
            continue
        fi
        if printf '%s\n' "$tree" | grep -q "^${other} v"; then
            echo "FAIL: ${crate} depends on ${other}" >&2
            status=1
        fi
    done
done

if [ "$status" -eq 0 ]; then
    echo "dependency graph conforms to NFR-ARCH-1"
fi

exit "$status"
