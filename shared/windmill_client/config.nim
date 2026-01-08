import std/[os, strutils]

type
  WindmillConfig* = object
    windmillHost*: string
    windmillPort*: string
    windmillBaseUrl*: string
    windmillApiBaseUrl*: string
    windmillWorkspace*: string
    windmillFolder*: string  # Optional: only needed for hash-based execution
    windmillEmail*: string
    windmillPassword*: string
    maxRetries*: int
    retryDelay*: int

proc loadWindmillConfig*(): WindmillConfig =
  ## Load Windmill configuration from environment variables
  # Use WINDMILL_BASE_URL if available, otherwise fall back to host/port pattern
  let baseUrl = getEnv("WINDMILL_BASE_URL", "")
  
  if baseUrl != "":
    # Parse baseUrl like "http://localhost:8001"
    let parts = baseUrl.split(":")
    let host = if parts.len >= 2: parts[1].replace("/", "") else: "localhost"
    let port = if parts.len >= 3: parts[2] else: "8001"
    
    result.windmillHost = host
    result.windmillPort = port
    result.windmillBaseUrl = baseUrl
  else:
    # Fallback to individual environment variables or defaults
    let host = getEnv("WINDMILL_APP_SERVICE_HOST", "windmill-app")
    let port = getEnv("WINDMILL_APP_SERVICE_PORT", "8000")
    
    result.windmillHost = host
    result.windmillPort = port
    result.windmillBaseUrl = "http://" & host & ":" & port
  
  result.windmillWorkspace = getEnv("WINDMILL_WORKSPACE", "godon")
  result.windmillFolder = getEnv("WINDMILL_FOLDER", "controller")
  result.windmillEmail = getEnv("WINDMILL_EMAIL", "admin@windmill.dev")
  result.windmillPassword = getEnv("WINDMILL_PASSWORD", "changeme")
  result.maxRetries = parseInt(getEnv("WINDMILL_MAX_RETRIES", "30"))
  result.retryDelay = parseInt(getEnv("WINDMILL_RETRY_DELAY", "2"))

  # Windmill API URL pattern for script execution
  result.windmillApiBaseUrl = result.windmillBaseUrl & "/api/w/" & result.windmillWorkspace & "/jobs/run_wait_result/p"