# Godon API Testing

This directory contains tests for the Godon API, designed to run both locally and in CI/CD using container-based testing.

## Test Structure

```
tests/
├── test_utils.nim          # Test utilities and helpers
├── test_handlers.nim       # Unit tests for API handlers
├── test_integration.nim    # Integration tests (requires Windmill service)
├── windmill-openapi.yaml  # OpenAPI spec for Prism mock Windmill service
└── README.md               # This file
```

## Container-Based Testing

Tests are compiled into the Nix container and run via the built-in test runner:

### **Container Test Runner**
```bash
# Inside the container
/app/tests/run_tests [unit|integration|all]
```

## Test Types

### **Unit Tests** (`test_handlers.nim`)
- Tests individual handler functions in isolation
- Validates request/response logic without external dependencies
- UUID validation, error formatting, JSON parsing

### **Integration Tests** (`test_integration.nim`)
- Full HTTP endpoint testing
- Requires Windmill service (real or mock via Prism)
- Tests actual API behavior and integration points

## Windmill Service Mock

The tests use **Prism** to mock the Windmill API based on `windmill-openapi.yaml`:

### **OpenAPI Specification**
- **File**: `windmill-openapi.yaml`
- **Purpose**: Defines Windmill API endpoints that our godon-api calls
- **Usage**: Prism generates mock responses based on this spec

### **Mock Endpoints**
- `POST /api/auth/login` - Authentication
- `POST /breeders_get` - Get all breeders
- `POST /breeder_create` - Create new breeder
- `POST /breeder_get` - Get specific breeder
- `POST /breeder_delete` - Delete breeder

### **Environment Configuration**
Tests read Windmill endpoints from environment variables:
```bash
WINDMILL_BASE_URL=http://localhost:8001
WINDMILL_API_BASE_URL=http://localhost:8001
```

## CI/CD Integration

### **GitHub Actions**
- **Service Container**: Prism runs as GitHub Actions service
- **Container Tests**: Tests executed inside built container
- **Network**: `--network host` for container-to-service communication

### **Workflow Steps**
1. Build container with Nix (includes compiled tests)
2. Start Prism service container
3. Run unit tests in container
4. Run integration tests in container
5. Test container as running service

## Test Coverage

### **Unit Test Coverage**
- ✅ HTTP method routing logic
- ✅ Request body validation
- ✅ UUID format validation
- ✅ Error response formatting
- ✅ JSON parsing edge cases
- ✅ Parameter validation

### **Integration Test Coverage**
- ✅ Health endpoint (`/health`)
- ✅ Root endpoint (`/`)
- ✅ CORS preflight (`OPTIONS`)
- ✅ Breeder endpoints (`/v0/breeders`)
- ✅ Response header validation
- ✅ Windmill service integration

### **OpenAPI Contract Coverage**
- ✅ Response schema validation
- ✅ Error format compliance
- ✅ Status code verification
- ✅ Header validation


## Adding New Tests

### **Unit Tests**
```nim
# Add to test_handlers.nim
test "new feature should work":
  let request = Request()
  let (code, body) = handleNewFeature(request)
  check code == Http200
```

### **Integration Tests**
```nim
# Add to test_integration.nim
test "new endpoint works":
  let response = client.get(baseUrl & "/new-endpoint")
  check response.status == "200 OK"
```

### **Test Utilities**
```nim
# Add to test_utils.nim
proc createTestData*(): JsonNode =
  result = %*{"test": "data"}
```

## Troubleshooting

### **Debug Mode**
```bash
# Run tests with verbose output
docker run --rm --network host \
  -e WINDMILL_BASE_URL=http://localhost:8001 \
  godon-api:dev /app/tests/run_tests all --verbose
```
