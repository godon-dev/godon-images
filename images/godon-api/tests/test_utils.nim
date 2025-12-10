import std/[unittest, json, times, random, os]
import handlers, types, windmill_client, config

# Test configuration that reads Windmill endpoints from environment variables
proc newTestConfig*(): Config =
  let baseUrl = getEnv("WINDMILL_BASE_URL", "http://localhost:8001")
  let apiUrl = getEnv("WINDMILL_API_BASE_URL", "http://localhost:8001")
  
  echo "Using Windmill configuration:"
  echo "  Base URL: ", baseUrl
  echo "  API URL: ", apiUrl
  
  Config(
    windmillBaseUrl: baseUrl,
    windmillApiBaseUrl: apiUrl
  )

# Test helper function to check if Windmill service is available
proc isWindmillServiceRunning*(): bool =
  ## Check if Windmill service is running and accessible
  let config = newTestConfig()
  let testUrl = config.windmillApiBaseUrl & "/api/auth/login"
  
  try:
    let client = newHttpClient()
    discard client.getContent(testUrl)
    client.close()
    echo "✅ Windmill service is available at: ", testUrl
    result = true
  except:
    echo "⚠️  Windmill service not available at: ", testUrl
    echo "Tests requiring Windmill integration will fail or be skipped"
    result = false

# Test data helpers
proc createTestBreeder*(name: string; config: JsonNode = nil): Breeder =
  ## Create a test breeder object for unit testing
  let timestamp = now().utc().format("yyyy-MM-dd'T'HH:mm:ss'Z'")
  result = Breeder(
    id: "550e8400-e29b-41d4-a716-44665544" & toHex(rand(9999).int, 4),
    name: name,
    status: "active",
    createdAt: timestamp,
    config: if config != nil: config else: %*{}
  )