#!/bin/sh
# Scaffolds crates/svc-<name> for an AWS SDK crate and wires it into the
# workspace and the binary. Usage: scripts/new-service.sh <name> <sdk-crate> <version>
# e.g. scripts/new-service.sh s3 aws-sdk-s3 1.152
set -eu
name=$1 sdk=$2 version=$3
root=$(cd "$(dirname "$0")/.." && pwd)
dir="$root/crates/svc-$name"
pkg="fast-aws-svc-$name"
[ -e "$dir" ] && { echo "$dir exists" >&2; exit 1; }
mkdir -p "$dir/src"
cat > "$dir/Cargo.toml" <<TOML
[package]
name = "$pkg"
description = "$name resource kinds for fast-aws"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
async-trait.workspace = true
$sdk.workspace = true
fast-aws-core.workspace = true
serde_json.workspace = true

[dev-dependencies]
$sdk = { workspace = true, features = ["test-util"] }
aws-smithy-mocks.workspace = true
tokio.workspace = true

[lints]
workspace = true
TOML
printf '//! %s resource kinds.\n\n/// Registers every %s resource kind.\npub fn register(_registry: &mut fast_aws_core::Registry) {}\n' "$name" "$name" > "$dir/src/lib.rs"
python3 - "$root" "$pkg" "$name" "$sdk" "$version" <<'PY'
import re, sys
root, pkg, name, sdk, version = sys.argv[1:]
p = f"{root}/Cargo.toml"; s = open(p).read()
s = s.replace('fast-aws-svc-ec2 = { path = "crates/svc-ec2" }\n',
              f'fast-aws-svc-ec2 = {{ path = "crates/svc-ec2" }}\n{pkg} = {{ path = "crates/svc-{name}" }}\n', 1)
s = s.replace('aws-runtime = "1.9"\n', f'aws-runtime = "1.9"\n{sdk} = "{version}"\n', 1)
open(p, "w").write(s)
p = f"{root}/crates/fast-aws/Cargo.toml"; s = open(p).read()
s = s.replace('fast-aws-svc-ec2.workspace = true\n', f'fast-aws-svc-ec2.workspace = true\n{pkg}.workspace = true\n', 1)
open(p, "w").write(s)
p = f"{root}/crates/fast-aws/src/main.rs"; s = open(p).read()
# Append after the last registration, so the palette lists services in the
# order they were added.
s = s.replace('\n    registry\n}', f'\n    {pkg.replace("-", "_")}::register(&mut registry);\n    registry\n}}', 1)
open(p, "w").write(s)
PY
echo "created $dir"
