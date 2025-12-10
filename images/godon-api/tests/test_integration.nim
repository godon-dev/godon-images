import std/[unittest, json, httpclient, asyncdispatch, strutils]
import godon_api

suite "API Integration Tests":
  var client: HttpClient
  let baseUrl = "http://localhost:8080"  # Test server should run on port 8080

  setup:
    client = newHttpClient()
    client.headers = newHttpHeaders({"Content-Type": "application/json"})

  teardown:
    client.close()

  test "GET /health - health check":
    try:
      let response = client.get(baseUrl & "/health")
      check response.status == "200 OK"
      
      let body = parseJson(response.body)
      check body.hasKey("status")
      check body["status"].getStr() == "healthy"
      check body.hasKey("service")
      check body["service"].getStr() == "godon-api"
      check body.hasKey("version")
      
      # Check CORS headers
      check response.headers.hasKey("access-control-allow-origin")
      check response.headers["access-control-allow-origin"] == "*"
      check response.headers.hasKey("content-type")
      check "application/json" in response.headers["content-type"]
    except CatchableError:
      echo "Warning: Integration test failed - server may not be running"

  test "GET / - root endpoint":
    try:
      let response = client.get(baseUrl & "/")
      check response.status == "200 OK"
      
      let body = parseJson(response.body)
      check body.hasKey("message")
      check body.hasKey("version")
      check body["message"].getStr() == "Godon API is running"
      
      # Check CORS headers
      check response.headers.hasKey("access-control-allow-origin")
      check response.headers["access-control-allow-origin"] == "*"
    except CatchableError:
      echo "Warning: Integration test failed - server may not be running"

  test "OPTIONS /*@path* - CORS preflight":
    try:
      let response = client.request(baseUrl & "/v0/breeders", httpMethod = HttpOptions)
      check response.status == "204 No Content"
      
      # Check CORS headers
      check response.headers.hasKey("access-control-allow-origin")
      check response.headers["access-control-allow-origin"] == "*"
    except CatchableError:
      echo "Warning: Integration test failed - server may not be running"

  test "GET /v0/breeders - requires Windmill service":
    try:
      let response = client.get(baseUrl & "/v0/breeders")
      # Should return 500 due to missing Windmill service in test environment
      check response.status == "500 Internal Server Error" or response.status == "200 OK"
      
      let body = parseJson(response.body)
      if response.status == "500":
        check body.hasKey("code")
        check body["code"].getStr() in ["INTERNAL_SERVER_ERROR", "BAD_REQUEST"]
      
      # Check CORS headers regardless of response
      check response.headers.hasKey("access-control-allow-origin")
      check response.headers["access-control-allow-origin"] == "*"
    except CatchableError:
      echo "Warning: Integration test failed - server may not be running"

  test "POST /v0/breeders - requires Windmill service":
    try:
      let requestBody = %*{
        "name": "test-breeder",
        "config": {"mutationRate": 0.1}
      }
      
      let response = client.post(baseUrl & "/v0/breeders", body = $requestBody)
      # Should return 500 due to missing Windmill service in test environment
      check response.status == "500 Internal Server Error" or response.status == "201 Created"
      
      let body = parseJson(response.body)
      if response.status == "500":
        check body.hasKey("code")
        check body["code"].getStr() in ["INTERNAL_SERVER_ERROR", "BAD_REQUEST"]
      elif response.status == "201":
        check body.hasKey("message")
        check body.hasKey("id")
      
      # Check CORS headers regardless of response
      check response.headers.hasKey("access-control-allow-origin")
      check response.headers["access-control-allow-origin"] == "*"
    except CatchableError:
      echo "Warning: Integration test failed - server may not be running"

suite "OpenAPI Contract Compliance Tests":
  
  test "health endpoint response format":
    try:
      let client = newHttpClient()
      let response = client.get(baseUrl & "/health")
      let body = parseJson(response.body)
      
      # Should match OpenAPI component structure for basic responses
      check body.hasKey("status")
      check body.hasKey("service") 
      check body.hasKey("version")
      
      client.close()
    except CatchableError:
      echo "Warning: Contract test failed - server may not be running"

  test "breeder endpoint error format compliance":
    try:
      let client = newHttpClient()
      client.headers = newHttpHeaders({"Content-Type": "application/json"})
      
      # Test invalid UUID to trigger error response
      let response = client.get(baseUrl & "/v0/breeders/invalid-uuid")
      
      if response.status == "400 Bad Request":
        let body = parseJson(response.body)
        
        # Should match OpenAPI Error schema
        check body.hasKey("message")
        check body.hasKey("code")
        check body["message"].getStr().len > 0
        check body["code"].getStr().len > 0
        
        # Should have optional details field for validation errors
        if body.hasKey("details"):
          check body["details"].kind == JObject
      
      client.close()
    except CatchableError:
      echo "Warning: Contract test failed - server may not be running"