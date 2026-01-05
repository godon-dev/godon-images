#!/usr/bin/env bash

# Script that seeds the breeder flows and scripts into the windmill orchestration engine.
# Now orchestrates the Nim godon-seeder instead of using wmill CLI directly.

set -eEux
set -o pipefail
shopt -s inherit_errexit

# Set default environment variables
export WINDMILL_BASE_URL="${WINDMILL_BASE_URL:-windmill-app:8000}"
export WINDMILL_WORKSPACE="${WINDMILL_WORKSPACE:-godon}"
export WINDMILL_EMAIL="${WINDMILL_EMAIL:-admin@windmill.dev}"
export WINDMILL_PASSWORD="${WINDMILL_PASSWORD:-changeme}"
export CONTROLLER_REPO="${CONTROLLER_REPO:-https://github.com/godon-dev/godon-controller.git}"
export CONTROLLER_VERSION="${CONTROLLER_VERSION:-0.1.0}"
export BREEDER_REPO="${BREEDER_REPO:-https://github.com/godon-dev/godon-breeders.git}"
export BREEDER_VERSION="${BREEDER_VERSION:-0.1.0}"
export GODON_DIR="${GODON_DIR:-/var/lib/godon}"

# Path to the Nim godon-seeder binary (use PATH to find it)
GODON_SEEDER_BIN="${GODON_SEEDER_BIN:-godon_seeder}"

## Setup repositories using reusable function
setup_repo() {
    local repo_name="$1"
    local repo_url="$2"
    local repo_version="$3"
    local target_dir="${GODON_DIR}/${repo_name}"

    echo "Setting up ${repo_name} repo: ${repo_url} @ ${repo_version}"

    if [ -d "${target_dir}/.git" ]; then
        echo "✅ ${repo_name} repo exists, updating..."
        pushd "${target_dir}"
        git fetch --all --tags
        git checkout "${repo_version}" || git checkout -B "${repo_version}" "origin/${repo_version}"
        popd
    else
        echo "📥 Cloning ${repo_name} repo..."
        mkdir -p "${GODON_DIR}"
        git clone "${repo_url}" "${target_dir}" || echo "⚠️  ${repo_name} clone failed"
        pushd "${target_dir}"
        git fetch -a
        git checkout -B "${repo_version}" "${repo_version}"
        popd
    fi
}

## Setup Controller Repository
echo "Setting up godon-controller repository..."
setup_repo "godon-controller" "${CONTROLLER_REPO}" "${CONTROLLER_VERSION}"

## Setup Breeder Repository
echo "Setting up godon-breeders repository..."
setup_repo "godon-breeders" "${BREEDER_REPO}" "${BREEDER_VERSION}"

echo "✅ All repositories updated successfully"

## Seed Controller and Breeder Logic using Nim seeder
echo "Starting component deployment with godon-seeder"

# Call the Nim seeder with the controller and breeder directories
"${GODON_SEEDER_BIN}" \
    --verbose \
    "${GODON_DIR}/godon-controller" \
    "${GODON_DIR}/godon-breeders/breeder/linux_network_stack"

echo "✅ Godon seeding completed successfully!"