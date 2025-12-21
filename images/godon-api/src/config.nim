import std/[os, strutils]

type
  Config* = object
    port*: int
    windmillBaseUrl*: string
    windmillApiBaseUrl*: string
    windmillWorkspace*: string

proc loadConfig*(): Config =
  ## Load configuration from environment variables
  let portStr = getEnv("PORT", "8080")
  result.port = parseInt(portStr)
  result.windmillBaseUrl = getEnv("WINDMILL_BASE_URL", "http://localhost:8001")
  result.windmillApiBaseUrl = getEnv("WINDMILL_API_BASE_URL", "http://localhost:8001")
  result.windmillWorkspace = getEnv("WINDMILL_WORKSPACE", "godon")
