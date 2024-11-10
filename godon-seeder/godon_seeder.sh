#!/usr/bin/env bash

# Script that seeds the breeder flows and scripts into the windmill orchestration engine.

set -eEux
set -o pipefail
shopt -s inherit_errexit

echo "Seeding from ${GODON_VERSION}"

pushd "${GODON_DIR}"
git checkout -B "${GODON_VERSION}" "${GODON_VERSION}"

for breeder in $(ls -1 "breeder")
do
    echo "## performing seeding for ${breeder}"
    echo "## ${breeder} . DONE"
done

