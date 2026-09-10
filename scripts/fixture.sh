#!/usr/bin/env bash
# Throwaway fixture used to smoke-test the binary by hand.
set -euo pipefail

ROOT="${TMPDIR%/}/rb-fixture"
rm -rf "$ROOT"
mkdir -p "$ROOT"

git_init() {
  git -C "$1" init -q
  git -C "$1" -c user.email=t@example.com -c user.name=t add -A
  git -C "$1" -c user.email=t@example.com -c user.name=t commit -qm init
}

# 1. Rust project, target/ ignored, last touched long ago.
P="$ROOT/old-rust"
mkdir -p "$P/src" "$P/target/debug"
printf '[package]\nname="old"\nversion="0.1.0"\n' > "$P/Cargo.toml"
printf 'fn main() {}\n' > "$P/src/main.rs"
printf '/target\n' > "$P/.gitignore"
dd if=/dev/zero of="$P/target/debug/blob" bs=1m count=12 2>/dev/null
git_init "$P"
touch -t 202401010000 "$P/Cargo.toml" "$P/src/main.rs" "$P/.gitignore"

# 2. Node project: node_modules ignored, dist committed on purpose.
P="$ROOT/web-app"
mkdir -p "$P/node_modules/left" "$P/dist"
printf '{"name":"web"}\n' > "$P/package.json"
printf 'node_modules\n' > "$P/.gitignore"
dd if=/dev/zero of="$P/node_modules/left/blob" bs=1m count=7 2>/dev/null
dd if=/dev/zero of="$P/dist/bundle.js" bs=1m count=3 2>/dev/null
git_init "$P"
touch -t 202403150000 "$P/package.json" "$P/.gitignore" "$P/dist/bundle.js"

# 3. Python project with a nested __pycache__ and a venv.
P="$ROOT/scripts/etl"
mkdir -p "$P/pkg/sub/__pycache__" "$P/.venv/lib"
printf '[project]\nname="etl"\n' > "$P/pyproject.toml"
printf 'touch\n' > "$P/pkg/sub/mod.py"
printf '.venv\n__pycache__/\n' > "$P/.gitignore"
dd if=/dev/zero of="$P/.venv/lib/blob" bs=1m count=5 2>/dev/null
dd if=/dev/zero of="$P/pkg/sub/__pycache__/mod.pyc" bs=1k count=800 2>/dev/null
git_init "$P"
touch -t 202506010000 "$P/pyproject.toml" "$P/pkg/sub/mod.py" "$P/.gitignore"

# 4. Rust project outside any git repository.
P="$ROOT/no-git"
mkdir -p "$P/target"
printf '[package]\nname="nogit"\nversion="0.1.0"\n' > "$P/Cargo.toml"
dd if=/dev/zero of="$P/target/blob" bs=1m count=4 2>/dev/null
touch -t 202412200000 "$P/Cargo.toml"

# 5. Fresh project that must be filtered out by the age threshold.
P="$ROOT/current-work"
mkdir -p "$P/src" "$P/target"
printf '[package]\nname="cur"\nversion="0.1.0"\n' > "$P/Cargo.toml"
printf 'fn main() {}\n' > "$P/src/main.rs"
printf '/target\n' > "$P/.gitignore"
dd if=/dev/zero of="$P/target/blob" bs=1m count=9 2>/dev/null
git_init "$P"

echo "$ROOT"
