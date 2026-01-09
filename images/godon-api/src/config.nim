import std/[os, strutils]

type
  Config* = object
    port*: int
    windmillBaseUrl*: string
    windmillApiBaseUrl*: string
    windmillWorkspace*: string
    windmillFolder*: string

proc loadConfig*(): Config =
  ## Load configuration from environment variables
  let portStr = getEnv("PORT", "8080")
  result.port = parseInt(portStr)
  result.windmillBaseUrl = getEnv("WINDMILL_BASE_URL", "http://localhost:8000/api")
  result.windmillWorkspace = getEnv("WINDMILL_WORKSPACE", "godon")
  result.windmillFolder = getEnv("WINDMILL_FOLDER", "controller")

  # Construct the full Windmill API URL following the original pattern
  # WINDMILL_API_BASE_URL=f"{WINDMILL_BASE_URL}/w/{WINDMILL_WORKSPACE}/jobs/run_wait_result/p/f/{WINDMILL_FOLDER}"
  # Note: WINDMILL_BASE_URL now includes /api, so we don't add it here
  result.windmillApiBaseUrl = result.windmillBaseUrl & "/w/" & result.windmillWorkspace & "/jobs/run_wait_result/p/f/" & result.windmillFolder
