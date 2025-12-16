import os, strutils

###
type
  Config* = object
    windmillHost*: string
    windmillPort*: string
    windmillWorkspace*: string
    windmillFolder*: string
    windmillBaseUrl*: string
    windmillApiBaseUrl*: string

proc loadConfig*(): Config =
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
    let host = getEnv("WINDMILL_APP_SERVICE_HOST", "localhost")
    let port = getEnv("WINDMILL_APP_SERVICE_PORT", "8000")
    
    result.windmillHost = host
    result.windmillPort = port
    result.windmillBaseUrl = "http://" & host & ":" & port
  
  result.windmillWorkspace = "godon"
  result.windmillFolder = "controller"
  # Windmill API URL pattern for script execution: {BASE_URL}/api/w/{WORKSPACE}/jobs/run_wait_result/p/
  # The full path (f/{folder}/{flow_name}) will be constructed and URL-encoded in runFlow()
  result.windmillApiBaseUrl = result.windmillBaseUrl & "/w/" & result.windmillWorkspace & "/jobs/run_wait_result/p"
