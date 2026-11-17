#!/usr/bin/env bash
# Dependency check of the hito 3 backend candidates. For each binary it
# measures:
#   - whether `cargo +1.91 check` passes and, if not, the first error line and
#     the line that names the offending crate;
#   - how many crates it brings (normal edges, each name and version once);
#   - which crates it brings in more than one version;
#   - how long a release build from clean takes, in its own target directory,
#     with the 1-minute load average at its start and end;
#   - the size of the binary, and how much it adds over `baseline`'s.
# and writes docs/decisions/hito-3-backend/compat.json.
#
# Usage: bash spikes/hito-3/compat.sh [--repeat N] [--provisional "<reason>"]
#   --repeat N        build each candidate N times from clean (default 1), in an
#                     order rotated on each round; `release_build_s` is the
#                     minimum and every sample is kept.
#   --provisional R   the binaries are not the real ones yet: the JSON says why
#                     in `provisional`, and binary sizes are written as null.
#
# Needs Linux with GNU coreutils, the MSRV toolchain
# (`rustup toolchain install 1.91`) and python3. The build timings depend on
# the host's load: compare them only within one run.
set -euo pipefail

msrv=1.91
candidates=(baseline kad-libp2p kad-iroh mainline)

usage() {
    echo "usage: bash spikes/hito-3/compat.sh [--repeat N] [--provisional \"<reason>\"]" >&2
    exit 2
}

repeat=1
provisional=""
while [ $# -gt 0 ]; do
    case "$1" in
        --repeat)
            [ $# -ge 2 ] || usage
            repeat="$2"
            shift 2
            ;;
        --provisional)
            [ $# -ge 2 ] && [ -n "$2" ] || usage
            provisional="$2"
            shift 2
            ;;
        *) usage ;;
    esac
done
if ! [[ "$repeat" =~ ^[1-9][0-9]*$ ]]; then
    echo "compat.sh: --repeat needs a positive integer, not '$repeat'" >&2
    exit 2
fi

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
out="$repo/docs/decisions/hito-3-backend/compat.json"
target="$here/target/compat"
work="$target/data"

export CARGO_TERM_COLOR=never
# A clean build must really compile everything: empty values also override a
# wrapper set in a cargo config file.
export RUSTC_WRAPPER= RUSTC_WORKSPACE_WRAPPER=

cd "$here"

if ! cargo "+$msrv" --version >/dev/null 2>&1; then
    echo "compat.sh: the $msrv toolchain is missing; install it with: rustup toolchain install $msrv" >&2
    exit 1
fi
if ! command -v python3 >/dev/null 2>&1; then
    echo "compat.sh: python3 is missing; it writes the JSON" >&2
    exit 1
fi

rm -rf "$work"
mkdir -p "$work" "$(dirname "$out")"

load_1m() { cut -d ' ' -f 1 /proc/loadavg; }

for id in "${candidates[@]}"; do
    pkg="spike-$id"
    dir="$work/$id"
    mkdir -p "$dir"

    echo "== $id: cargo +$msrv check" >&2
    if CARGO_TARGET_DIR="$target/msrv" cargo "+$msrv" check --locked -p "$pkg" \
        >"$dir/msrv.log" 2>&1; then
        echo pass >"$dir/msrv"
    else
        echo fail >"$dir/msrv"
    fi

    echo "== $id: cargo tree" >&2
    # A crate seen again is printed with " (*)": strip it so that it counts once.
    cargo tree --locked -p "$pkg" -e normal --prefix none \
        | sed 's/ (\*)$//' | sort -u | wc -l >"$dir/crates"
    # The duplicated crates themselves, without the trees that bring them.
    cargo tree --locked -p "$pkg" -d -e normal --depth 0 >"$dir/duplicates"
done

n=${#candidates[@]}
for ((round = 0; round < repeat; round++)); do
    for ((i = 0; i < n; i++)); do
        # Each round starts one candidate later, so that none is always first.
        id="${candidates[(round + i) % n]}"
        pkg="spike-$id"
        dir="$work/$id"
        echo "== $id: cargo build --release, from clean (round $((round + 1)) of $repeat)" >&2
        rm -rf "$target/build/$id"
        load_start="$(load_1m)"
        start="$(date +%s.%N)"
        CARGO_TARGET_DIR="$target/build/$id" cargo build --release --locked -p "$pkg" \
            >"$dir/build.log" 2>&1 || {
            echo "compat.sh: the release build of $pkg failed, see $dir/build.log" >&2
            exit 1
        }
        end="$(date +%s.%N)"
        echo "$start $end $load_start $(load_1m)" >>"$dir/builds"
        stat -c %s "$target/build/$id/release/$pkg" >"$dir/binary_bytes"
    done
done

commit="$(git -C "$repo" rev-parse --short HEAD)"
# Dirty when what is measured differs from the commit.
if [ -n "$(git -C "$repo" status --porcelain -- Cargo.toml Cargo.lock crates spikes/hito-3)" ]; then
    commit="$commit-dirty"
fi

python3 -I - "$work" "$out" "$repo" "$(rustc --version)" "$(rustc "+$msrv" --version)" \
    "$commit" "$(nproc)" "$provisional" "${candidates[@]}" <<'EOF'
import datetime
import json
import os
import sys

work, out, repo, rustc, rustc_msrv, commit, cpus, provisional, *candidates = sys.argv[1:]

# Paths of this host never reach the JSON: the longest first, so that a
# CARGO_HOME under HOME keeps its own name.
PATHS = sorted(
    (
        (path, name)
        for path, name in [
            (repo, "."),
            (os.environ.get("CARGO_HOME", ""), "$CARGO_HOME"),
            (os.environ.get("RUSTUP_HOME", ""), "$RUSTUP_HOME"),
            (os.environ.get("HOME", ""), "~"),
        ]
        if path and path != "/"
    ),
    key=lambda pair: len(pair[0]),
    reverse=True,
)


def sanitize(text):
    for path, name in PATHS:
        text = text.replace(path, name)
    return text


def read(id_, name):
    with open(os.path.join(work, id_, name)) as f:
        return f.read().strip()


def msrv_error(log):
    """The first error line, and the line that names the crate: the first one
    saying `requires rustc`, or else the next non-empty one."""
    lines = [line.strip() for line in log.splitlines()]
    first = next((i for i, line in enumerate(lines) if line.startswith("error")), None)
    if first is None:
        rest = [line for line in lines if line]
        return sanitize(rest[-1]) if rest else "no output"
    after = lines[first + 1 :]
    named = next((line for line in after if "requires rustc" in line), None)
    if named is None:
        named = next((line for line in after if line), None)
    return sanitize(lines[first] if named is None else f"{lines[first]} | {named}")


def duplicates(text):
    """`name vA, vB` for every name `cargo tree -d` prints in two versions."""
    versions = {}
    for line in text.splitlines():
        parts = line.split()
        if len(parts) >= 2:
            seen = versions.setdefault(parts[0], [])
            if parts[1] not in seen:
                seen.append(parts[1])
    return [f"{name} {', '.join(vs)}" for name, vs in versions.items() if len(vs) > 1]


def builds(id_):
    """Each sample's seconds and its 1-minute load average at start and end."""
    samples = []
    for line in read(id_, "builds").splitlines():
        start, end, load_start, load_end = (float(x) for x in line.split())
        samples.append((round(end - start, 1), [load_start, load_end]))
    return samples


result = {
    "date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "rustc": rustc,
    "rustc_msrv": rustc_msrv,
    "commit": commit,
    "host_cpus": int(cpus),
}
if provisional:
    result["provisional"] = provisional
result["candidates"] = {}

baseline_bytes = int(read("baseline", "binary_bytes"))
for id_ in candidates:
    msrv = read(id_, "msrv")
    samples = builds(id_)
    binary_bytes = int(read(id_, "binary_bytes"))
    result["candidates"][id_] = {
        "msrv_1_91": msrv,
        "msrv_error": None if msrv == "pass" else msrv_error(read(id_, "msrv.log")),
        "crates": int(read(id_, "crates")),
        "duplicates": duplicates(read(id_, "duplicates")),
        "release_build_s": min(s for s, _ in samples),
        "release_build_samples_s": [s for s, _ in samples],
        "release_build_load_1m": [load for _, load in samples],
        "binary_bytes": None if provisional else binary_bytes,
        "binary_bytes_over_baseline": None if provisional else binary_bytes - baseline_bytes,
    }

tmp = out + ".tmp"
with open(tmp, "w") as f:
    json.dump(result, f, indent=2)
    f.write("\n")
os.replace(tmp, out)
print(f"compat.sh: wrote {os.path.relpath(out)}", file=sys.stderr)
EOF
