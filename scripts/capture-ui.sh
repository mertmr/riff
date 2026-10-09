#!/usr/bin/env bash
# Captures the real native wgpu renderer. Fixtures are isolated from saved collections.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --features ui-qa
capture_dir="${1:-target/ui-captures}"
mkdir -p "$capture_dir"
for size in 1440x900 1280x800 1024x640; do
  for view in home home-loading home-error home-empty home-long search library favorites queue radio radio-loading radio-error loading error long missing; do
    RIFF_CAPTURE="$capture_dir/$view-$size.png" RIFF_SIZE="$size" RIFF_VIEW="$view" RIFF_FIXTURES=1 target/debug/riff-rs
  done
  RIFF_CAPTURE="$capture_dir/first-launch-$size.png" RIFF_SIZE="$size" target/debug/riff-rs
  RIFF_CAPTURE="$capture_dir/favorites-empty-$size.png" RIFF_SIZE="$size" RIFF_VIEW=favorites target/debug/riff-rs
done
