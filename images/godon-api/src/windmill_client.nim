import std/[httpclient, json, strformat, tables, uri]
import config, types

type
  WindmillClient* = object
    config: Config
    token: string

proc login*(client: var WindmillClient) =
  let url = &"{client.config.windmillBaseUrl}/auth/login"
  let payload = %* {
    "email": "admin@windmill.dev",
    "password": "changeme"
  }
  
  var http = newHttpClient()
  http.headers = newHttpHeaders({"Content-Type": "application/json"})
  
  try:
    let response = http.post(url, $payload)
    if response.code != Http200:
      raise newException(ValueError, "Failed to login to Windmill: " & response.status)
    
    client.token = response.body
  finally:
    http.close()

proc newWindmillClient*(config: Config): WindmillClient =
  result.config = config
  result.login()

proc runFlow*(client: WindmillClient, flowPath: string, args: JsonNode = nil): JsonNode =
  ## Run a Windmill flow by path and wait for result
  ## This is the main method for executing custom controller scripts
  ## Uses the exact same URL pattern as the original Python implementation
  let fullPath = "f/" & client.config.windmillFolder & "/" & flowPath
  let encodedPath = encodeUrl(fullPath)  # URL encode the path containing slashes
  let url = &"{client.config.windmillApiBaseUrl}/{encodedPath}"
  
  var http = newHttpClient()
  http.headers = newHttpHeaders({
    "Content-Type": "application/json",
    "Authorization": "Bearer " & client.token
  })
  
  try:
    var body = ""
    if args != nil:
      body = $args
    else:
      body = "{}"
    
    let response = http.post(url, body)
    if response.code != Http200:
      raise newException(ValueError, "Windmill flow execution failed: " & response.status)
    
    result = parseJson(response.body)
  finally:
    http.close()

proc getBreeders*(client: WindmillClient): seq[Breeder] =
  ## Execute the custom 'breeders_get' controller flow
  let response = client.runFlow("breeders_get")
  if response.hasKey("breeders"):
    result = parseBreedersFromJson(response["breeders"])
  else:
    result = @[]

proc createBreeder*(client: WindmillClient, config: JsonNode): string =
  ## Execute the custom 'breeder_create' controller flow
  let args = %* {"breeder_config": config}
  let response = client.runFlow("breeder_create", args)
  if response.hasKey("breeder_id"):
    result = response["breeder_id"].getStr()
  else:
    raise newException(ValueError, "No breeder_id returned from flow")

proc getBreeder*(client: WindmillClient, breederId: string): Breeder =
  ## Execute the custom 'breeder_get' controller flow
  let args = %* {"breeder_id": breederId}
  let response = client.runFlow("breeder_get", args)
  if response.hasKey("breeder_data"):
    result = parseBreederFromJson(response["breeder_data"])
  else:
    raise newException(ValueError, "No breeder_data returned from flow")

proc deleteBreeder*(client: WindmillClient, breederId: string) =
  ## Execute the custom 'breeder_delete' controller flow
  let args = %* {"breeder_id": breederId}
  discard client.runFlow("breeder_delete", args)