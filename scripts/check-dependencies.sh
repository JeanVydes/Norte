#!/usr/bin/env bash
#
# NFR-ARCH-1: dependencies point toward core only.
#
# - norte-core never pulls in HTTP, database, vendor, concrete-tool or harness
#   code. The core defines what a conversation *is* and what a provider must
#   implement; an agent runtime is one way to drive those, not part of them.
# - No adapter depends on another adapter, nor on the harness. A provider wire
#   is a consumer of the core like the harness is, and neither knows the other.
# - norte-testing is the one exception, and a deliberate one: it ships the
#   conformance suite for the ports the *harness* owns (`Store` among them), so
#   its library names those types. It is a test helper, never a runtime
#   dependency of a shipping crate.
#
# The check reads the normal (non-dev, non-build) dependency tree, so crates
# that are only dev-dependencies of a conformance suite do not count.
set -euo pipefail

# Crates that must not depend on each other, and none of which may depend on the
# harness.
adapters="norte-anthropic norte-openai norte-gemini norte-tools norte-media norte-mcp"

core_forbidden='^(norte-(http|harness|anthropic|openai|gemini|tools|media|testing|mcp)) v|^(reqwest|hyper|axum|tower-http|sqlx|diesel|mongodb|redis|rusqlite|aws-sdk|tokio-postgres) '

status=0

core_tree="$(cargo tree -p norte-core -e normal --prefix none)"
if printf '%s\n' "$core_tree" | grep -Eq "$core_forbidden"; then
    echo "FAIL: norte-core depends on non-core code:" >&2
    printf '%s\n' "$core_tree" | grep -E "$core_forbidden" >&2
    status=1
fi

# The harness may depend on the core and on nothing else in the workspace.
harness_tree="$(cargo tree -p norte-harness -e normal --prefix none)"
if printf '%s\n' "$harness_tree" | grep -Eq '^norte-(http|anthropic|openai|gemini|tools|media|testing|mcp) v'; then
    echo "FAIL: norte-harness depends on an adapter:" >&2
    printf '%s\n' "$harness_tree" | grep -E '^norte-(http|anthropic|openai|gemini|tools|media|testing|mcp) v' >&2
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
    if printf '%s\n' "$tree" | grep -q '^norte-harness v'; then
        echo "FAIL: ${crate} depends on norte-harness" >&2
        status=1
    fi
done

if [ "$status" -eq 0 ]; then
    echo "dependency graph conforms to NFR-ARCH-1"
fi

exit "$status"
