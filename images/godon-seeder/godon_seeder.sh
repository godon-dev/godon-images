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
export GODON_VERSION="${GODON_VERSION:-main}"
export GODON_DIR="${GODON_DIR:-/godon}"

# Path to the Nim godon-seeder binary (use PATH to find it)
GODON_SEEDER_BIN="${GODON_SEEDER_BIN:-godon_seeder}"

## Clone and Checkout the Relevant Scripts and Flows Version
echo "Updating godon repository to version ${GODON_VERSION}"

# Check if GODON_DIR exists and is a git repository
if [ -d "${GODON_DIR}/.git" ]; then
  echo "Repository exists, pulling latest changes..."
  pushd "${GODON_DIR}"
  git fetch --all --tags
  git checkout -B "${GODON_VERSION}" "origin/${GODON_VERSION}" 2>/dev/null || git checkout "${GODON_VERSION}"
  git pull origin "${GODON_VERSION}" 2>/dev/null || true
  popd
else
  echo "Repository not found, cloning..."
  rm -rf "${GODON_DIR}" 2>/dev/null || true
  git clone --depth 1 --branch "${GODON_VERSION}" https://github.com/godon-dev/godon.git "${GODON_DIR}" || echo "⚠️  Git clone failed, continuing anyway"
fi

echo "✅ Godon repository updated successfully"

## Seed Controller and Breeder Logic using Nim seeder
echo "Starting component deployment with godon-seeder"

# Call the Nim seeder with the controller and breeder directories
# The seeder will handle workspace creation and component deployment
"${GODON_SEEDER_BIN}" \
    --verbose \
    "${GODON_DIR}/controller" \
    "${GODON_DIR}/breeder/linux_network_stack"

echo "✅ Godon seeding completed successfully!"
