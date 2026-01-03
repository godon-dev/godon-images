#!/usr/bin/env bash

# Script that seeds the breeder flows and scripts into the windmill orchestration engine.
# Now orchestrates the Nim godon-seeder instead of using wmill CLI directly.

set -eEux
set -o pipefail
shopt -s inherit_errexit

# Set default environment variables
export WINDMILL_BASE_URL="${WINDMILL_BASE_URL:-windmill-app:8000}"
export WINDMILL_WORKSPACE="${WINDMILL_WORKSPACE:-godon-test3}"
export WINDMILL_EMAIL="${WINDMILL_EMAIL:-admin@windmill.dev}"
export WINDMILL_PASSWORD="${WINDMILL_PASSWORD:-changeme}"
export GODON_DIR="${GODON_DIR:-/godon}"

# Multi-repo configuration
export CONTROLLER_REPO="${CONTROLLER_REPO:-https://github.com/godon-dev/godon-controller.git}"
export CONTROLLER_VERSION="${CONTROLLER_VERSION:-main}"
export BREEDER_REPO="${BREEDER_REPO:-https://github.com/godon-dev/godon-breeders.git}"  
export BREEDER_VERSION="${BREEDER_VERSION:-main}"

# Path to the Nim godon-seeder binary (use PATH to find it)
GODON_SEEDER_BIN="${GODON_SEEDER_BIN:-godon_seeder}"

## Clone and Checkout the Relevant Scripts and Flows Version

# Reusable function for repo cloning/updating
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
        git checkout -B "${repo_version}" "origin/${repo_version}" 2>/dev/null || git checkout "${repo_version}"
        git pull origin "${repo_version}" 2>/dev/null || true
        popd
    else
        echo "📥 Cloning ${repo_name} repo..."
        rm -rf "${target_dir}" 2>/dev/null || true
        git clone --depth 1 --branch "${repo_version}" "${repo_url}" "${target_dir}" || echo "⚠️  ${repo_name} clone failed"
    fi
}

echo "Setting up multi-repo godon structure"
mkdir -p "${GODON_DIR}"

# Setup Controller Repo
setup_repo "controller" "${CONTROLLER_REPO}" "${CONTROLLER_VERSION}"

# Setup Breeder Repo  
setup_repo "breeders" "${BREEDER_REPO}" "${BREEDER_VERSION}"

echo "✅ Multi-repo structure updated successfully"

## Seed Controller and Breeder Logic using Nim seeder
echo "Starting component deployment with godon-seeder"

# Call the Nim seeder with the controller and breeder directories
# The seeder will handle workspace creation and component deployment
"${GODON_SEEDER_BIN}" \
    --verbose \
    "${GODON_DIR}/controller" \
    "${GODON_DIR}/breeders"

echo "✅ Godon seeding completed successfully!"
