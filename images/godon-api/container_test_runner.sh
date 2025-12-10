#!/bin/sh
# Container test runner for godon-api
# This script runs tests inside the container environment
# Usage: ./run_tests.sh [unit|integration|all]

set -e

# Colors for output
if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[1;33m'
    BLUE='\033[0;34m'
    NC='\033[0m' # No Color
else
    RED='' GREEN='' YELLOW='' BLUE='' NC=''
fi

log_info() { echo -e "${BLUE}ℹ️  $1${NC}"; }
log_success() { echo -e "${GREEN}✅ $1${NC}"; }
log_warning() { echo -e "${YELLOW}⚠️  $1${NC}"; }
log_error() { echo -e "${RED}❌ $1${NC}"; }

TEST_TYPE="${1:-all}"

# Check if test binaries exist
check_test_binaries() {
    if [ ! -f "/app/tests/test_handlers" ] && [ ! -f "/app/tests/test_integration" ]; then
        log_error "No test binaries found. Tests may not have been built into this container."
        log_info "Expected: /app/tests/test_handlers and/or /app/tests/test_integration"
        exit 1
    fi
}

# Run unit tests
run_unit_tests() {
    log_info "Running unit tests..."
    
    if [ ! -f "/app/tests/test_handlers" ]; then
        log_warning "Unit test binary not found: /app/tests/test_handlers"
        return 0
    fi
    
    if /app/tests/test_handlers; then
        log_success "Unit tests passed!"
        return 0
    else
        log_error "Unit tests failed!"
        return 1
    fi
}

# Run integration tests
run_integration_tests() {
    log_info "Running integration tests..."
    
    if [ ! -f "/app/tests/test_integration" ]; then
        log_warning "Integration test binary not found: /app/tests/test_integration"
        return 0
    fi
    
    # Check if Windmill service is available
    if ! curl -f "${WINDMILL_API_BASE_URL:-http://localhost:8001}/api/auth/login" >/dev/null 2>&1; then
        log_warning "Windmill service not available at: ${WINDMILL_API_BASE_URL:-http://localhost:8001}/api/auth/login"
        log_info "Integration tests will run but may fail without Windmill backend"
    fi
    
    # Start the API server in background for integration tests
    log_info "Starting godon-api server for integration tests..."
    /app/godon_api &
    API_PID=$!
    
    # Wait for API to be ready
    for i in $(seq 1 30); do
        if curl -f http://localhost:8080/health >/dev/null 2>&1; then
            log_info "Godon API is ready!"
            break
        fi
        if [ "$i" -eq 30 ]; then
            log_error "Godon API failed to start within 60 seconds"
            kill $API_PID 2>/dev/null || true
            return 1
        fi
        sleep 2
    done
    
    # Run integration tests
    if WINDMILL_BASE_URL="${WINDMILL_BASE_URL:-http://localhost:8001}" \
       WINDMILL_API_BASE_URL="${WINDMILL_API_BASE_URL:-http://localhost:8001}" \
       /app/tests/test_integration; then
        log_success "Integration tests passed!"
        TEST_RESULT=0
    else
        log_error "Integration tests failed!"
        TEST_RESULT=1
    fi
    
    # Clean up API server
    kill $API_PID 2>/dev/null || true
    return $TEST_RESULT
}

# Main execution
main() {
    log_info "Container-based test runner for godon-api"
    log_info "Test type: $TEST_TYPE"
    log_info "Windmill Base URL: ${WINDMILL_BASE_URL:-http://localhost:8001}"
    log_info "Windmill API URL: ${WINDMILL_API_BASE_URL:-http://localhost:8001}"
    
    check_test_binaries
    
    case $TEST_TYPE in
        "unit")
            run_unit_tests
            ;;
        "integration")
            run_integration_tests
            ;;
        "all")
            OVERALL_RESULT=0
            run_unit_tests || OVERALL_RESULT=1
            echo ""
            run_integration_tests || OVERALL_RESULT=1
            echo ""
            if [ $OVERALL_RESULT -eq 0 ]; then
                log_success "All tests passed! 🎉"
            else
                log_error "Some tests failed! ❌"
            fi
            exit $OVERALL_RESULT
            ;;
        *)
            log_error "Unknown test type: $TEST_TYPE"
            echo "Usage: $0 [unit|integration|all]"
            exit 1
            ;;
    esac
}

main "$@"