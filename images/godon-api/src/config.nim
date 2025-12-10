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
  let host = getEnv("WINDMILL_APP_SERVICE_HOST", "localhost")
  let port = getEnv("WINDMILL_APP_SERVICE_PORT", "8000")
  
  result.windmillHost = host
  result.windmillPort = port
  result.windmillWorkspace = "godon"
  result.windmillFolder = "controller"
  result.windmillBaseUrl = "http://" & host & ":" & port
  # Exact same URL pattern as Python implementation:
  # WINDMILL_API_BASE_URL=f"{WINDMILL_BASE_URL}/api/w/{WINDMILL_WORKSPACE}/jobs/run_wait_result/p/f/{WINDMILL_FOLDER}"
  result.windmillApiBaseUrl = result.windmillBaseUrl & "/api/w/" & result.windmillWorkspace & "/jobs/run_wait_result/p/f/" & result.windmillFolder
