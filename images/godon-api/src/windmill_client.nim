import std/[httpclient, json, strformat, tables]
import config, types

type
  WindmillClient* = object
    config: Config
    token: string

proc login*(client: var WindmillClient) =
  let url = &"{client.config.windmillBaseUrl}/api/auth/login"
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

proc makeRequest*(client: WindmillClient, path: string, payload: JsonNode = nil): JsonNode =
  let url = client.config.windmillApiBaseUrl & path
  var http = newHttpClient()
  http.headers = newHttpHeaders({
    "Content-Type": "application/json",
    "Authorization": "Bearer " & client.token
  })
  
  try:
    var body = ""
    if payload != nil:
      body = $payload
    
    let response = http.post(url, body)
    if response.code != Http200:
      raise newException(ValueError, "Windmill API request failed: " & response.status)
    
    result = parseJson(response.body)
  finally:
    http.close()

proc getBreeders*(client: WindmillClient): seq[Breeder] =
  let response = client.makeRequest("/breeders_get")
  result = parseBreedersFromJson(response["breeders"])

proc createBreeder*(client: WindmillClient, config: JsonNode): string =
  let payload = %* {"breeder_config": config}
  let response = client.makeRequest("/breeder_create", payload)
  result = response["breeder_id"].getStr()

proc getBreeder*(client: WindmillClient, breederId: string): Breeder =
  let payload = %* {"breeder_id": breederId}
  let response = client.makeRequest("/breeder_get", payload)
  result = parseBreederFromJson(response["breeder_data"])

proc deleteBreeder*(client: WindmillClient, breederId: string) =
  let payload = %* {"breeder_id": breederId}
  discard client.makeRequest("/breeder_delete", payload)