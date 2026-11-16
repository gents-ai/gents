#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
lake build Proofs.Conformance.BackendHealthAdmission
probe_cases=$(mktemp)
trap 'rm -f "$probe_cases"' EXIT
lake env lean --run tla/BackendFirstProbeCases.lean > "$probe_cases"
mv "$probe_cases" tla/BackendFirstProbeCases.tla
