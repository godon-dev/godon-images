#!/usr/bin/env bash

# Script that seeds the breeder flows and scripts into the windmill orchestration engine.

set -eEux
set -o pipefail
shopt -s inherit_errexit


## Set Windmill API Base URL
export WMILL_BASE_URL="windmill-app:8000"

## Logging in to Windmill to attain token
export WMILL_TOKEN="$(curl ${WMILL_BASE_URL}/api/auth/login \
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
wmill --base-url "http://${WMILL_BASE_URL}" --token "${WMILL_TOKEN}" workspace add \
      --create "${WMILL_WORKSPACE}" "${WMILL_WORKSPACE}" "http://${WMILL_BASE_URL}"

### Seed Controller Logic ###
pushd controller
wmill init

# create controller folder
mkdir -p f/controller

for script in $(ls -1 *.py)
do
    echo "## performing seeding for controller logic"

    mv "${script}" f/controller

    wmill --base-url "http://${WMILL_BASE_URL}" --token "${WMILL_TOKEN}" --workspace "${WMILL_WORKSPACE}" \
          script push f/controller/${script}

    echo "## Controller ... DONE"
done

popd

### Seed Breeder Logic ###

local_breeder_foler="breeder/linux_network_stack"

pushd "${local_breeder_foler}"
wmill init

wmill_breeder_folder="f/${local_breeder_foler}"

# create breeder folder
mkdir -p "${wmill_breeder_folder}"



echo "## performing seeding for linux_network_stack breeder logic"

for script in $(ls -1 *.py)
do

    mv "${script}" "${wmill_breeder_folder}"

    wmill --base-url "http://${WMILL_BASE_URL}" --token "${WMILL_TOKEN}" --workspace "${WMILL_WORKSPACE}" \
          script push  "${wmill_breeder_folder}/${script}"

done

for flow in $(ls -1 *.yaml)
do
    cp "$(pwd)/${flow}" "$(pwd)/flow.yaml"

    wmill --base-url "http://${WMILL_BASE_URL}" --token "${WMILL_TOKEN}" --workspace "${WMILL_WORKSPACE}" \
        flow push "$(pwd)/flow.yaml" "${wmill_breeder_folder}/${flow}"

done

echo "## linux_network_stack breeder ... DONE"
