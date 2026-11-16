#!/usr/bin/env bash
# Dependency check of the hito 3 backend candidates. For each binary it
# measures:
#   - whether `cargo +1.91 check` passes, with the first error line if not;
#   - how many crates it brings (normal edges, each name and version once);
#   - which crates it brings in more than one version;
#   - how long a release build from clean takes, in its own target directory,
#     with the 1-minute load average at its start and end;
#   - the size of the binary, and how much it adds over `baseline`'s.
# and writes docs/decisions/hito-3-backend/compat.json.
#
# Usage: bash spikes/hito-3/compat.sh   (from any directory)
#
# Needs the MSRV toolchain (`rustup toolchain install 1.91`) and python3. The
# build timings depend on the host's load: compare them only within one run.
set -euo pipefail

msrv=1.91
candidates=(baseline kad-libp2p kad-iroh mainline)

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
out="$repo/docs/decisions/hito-3-backend/compat.json"
target="$here/target/compat"
work="$target/data"

export CARGO_TERM_COLOR=never
# A clean build must really compile everything.
unset RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER

cd "$here"

if ! cargo "+$msrv" --version >/dev/null 2>&1; then
    echo "compat.sh: the $msrv toolchain is missing; install it with: rustup toolchain install $msrv" >&2
    exit 1
fi

rm -rf "$work"
mkdir -p "$work" "$(dirname "$out")"

load_1m() { cut -d ' ' -f 1 /proc/loadavg; }

# Paths of this host never reach the JSON.
sanitize() { sed -e "s|$repo|.|g" -e "s|$HOME|~|g"; }

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
        { grep -m 1 '^error' "$dir/msrv.log" || tail -n 1 "$dir/msrv.log"; } \
            | sanitize >"$dir/msrv_error"
    fi

    echo "== $id: cargo tree" >&2
    # A crate seen again is printed with " (*)": strip it so that it counts once.
    cargo tree --locked -p "$pkg" -e normal --prefix none \
        | sed 's/ (\*)$//' | sort -u | wc -l >"$dir/crates"
    # The duplicated crates themselves, without the trees that bring them.
    cargo tree --locked -p "$pkg" -d -e normal --depth 0 >"$dir/duplicates"

    echo "== $id: cargo build --release, from clean" >&2
    rm -rf "$target/build/$id"
    load_1m >"$dir/load_start"
    date +%s.%N >"$dir/build_start"
    CARGO_TARGET_DIR="$target/build/$id" cargo build --release --locked -p "$pkg" \
        >"$dir/build.log" 2>&1 || {
        echo "compat.sh: the release build of $pkg failed, see $dir/build.log" >&2
        exit 1
    }
    date +%s.%N >"$dir/build_end"
    load_1m >"$dir/load_end"
    stat -c %s "$target/build/$id/release/$pkg" >"$dir/binary_bytes"
done

commit="$(git -C "$repo" rev-parse --short HEAD)"
# Dirty when what is measured differs from the commit.
if [ -n "$(git -C "$repo" status --porcelain -- Cargo.toml Cargo.lock crates spikes/hito-3)" ]; then
    commit="$commit-dirty"
fi

python3 -I - "$work" "$out" "$(rustc --version)" "$(rustc "+$msrv" --version)" \
    "$commit" "$(nproc)" "${candidates[@]}" <<'EOF'
import datetime
import json
import os
import sys

work, out, rustc, rustc_msrv, commit, cpus, *candidates = sys.argv[1:]


def read(id_, name):
    path = os.path.join(work, id_, name)
    if not os.path.exists(path):
        return None
    with open(path) as f:
        return f.read().strip()


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


baseline_bytes = int(read("baseline", "binary_bytes"))
result = {
    "date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "rustc": rustc,
    "rustc_msrv": rustc_msrv,
    "commit": commit,
    "host_cpus": int(cpus),
    "candidates": {},
}
for id_ in candidates:
    binary_bytes = int(read(id_, "binary_bytes"))
    result["candidates"][id_] = {
        "msrv_1_91": read(id_, "msrv"),
        "msrv_error": read(id_, "msrv_error"),
        "crates": int(read(id_, "crates")),
        "duplicates": duplicates(read(id_, "duplicates")),
        "release_build_s": round(float(read(id_, "build_end")) - float(read(id_, "build_start")), 1),
        "release_build_load_1m": [float(read(id_, "load_start")), float(read(id_, "load_end"))],
        "binary_bytes": binary_bytes,
        "binary_bytes_over_baseline": binary_bytes - baseline_bytes,
    }

tmp = out + ".tmp"
with open(tmp, "w") as f:
    json.dump(result, f, indent=2)
    f.write("\n")
os.replace(tmp, out)
print(f"compat.sh: wrote {os.path.relpath(out)}", file=sys.stderr)
EOF
