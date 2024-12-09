#!/usr/bin/env bash

# Script that seeds the breeder flows and scripts into the windmill orchestration engine.

set -eEux
set -o pipefail
shopt -s inherit_errexit

## Logging in to Windmill to attain token
export WMILL_TOKEN="$(curl https://app.windmill.dev/api/auth/login \
                      --request POST \
                      --header 'Content-Type: application/json' \
                      --data '{
                      "email": "admin@windmill.dev",
                      "password": "changeme"
                       }')"

## Set Default Windmill Workspace
export WMILL_WORKSPACE="godon"

## Clone and Checkout the Relevant Scripts and Flows Version
echo "Seeding from ${GODON_VERSION}"
pushd "${GODON_DIR}"
git checkout -B "${GODON_VERSION}" "${GODON_VERSION}"

echo "Creating godon logic workspace"
wmill workspace add "${WMILL_WORKSPACE}" "${WMILL_WORKSPACE}" "https://app.windmill.dev/godon"

pushd controller
for script in $(ls -1)
do
    echo "## performing seeding for controller logic"
    wmill script bootstrap --summary "${script}" \
                           --description "${script}" \
                           "${script}" "python"

    wmill script push "${script}"
    echo "## Controller ... DONE"
done

