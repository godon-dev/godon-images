import std/[json, times]
import jsony

type
  Breeder* = object
    id*: string
    name*: string
    status*: string
    createdAt*: string
    config*: JsonNode

  BreederCreate* = object
    name*: string
    config*: JsonNode

  ApiResponse* = object
    message*: string
    code*: string
    details*: JsonNode

  Credential* = object
    id*: string
    name*: string
    credentialType*: string
    description*: string
    windmillVariable*: string
    createdAt*: string
    lastUsedAt*: string

# Custom JSON parsing functions
proc parseBreederFromJson*(json: JsonNode): Breeder =
  result = Breeder()
  if json.hasKey("id"):
    result.id = json["id"].getStr()
  if json.hasKey("name"):
    result.name = json["name"].getStr()
  if json.hasKey("status"):
    result.status = json["status"].getStr()
  if json.hasKey("createdAt"):
    result.createdAt = json["createdAt"].getStr()
  if json.hasKey("config"):
    result.config = json["config"]

proc parseBreedersFromJson*(json: JsonNode): seq[Breeder] =
  result = @[]
  if json.kind == JArray:
    for item in json.items:
      result.add(parseBreederFromJson(item))

proc parseCredentialFromJson*(json: JsonNode): Credential =
  result = Credential()
  if json.hasKey("id"):
    result.id = json["id"].getStr()
  if json.hasKey("name"):
    result.name = json["name"].getStr()
  if json.hasKey("credential_type"):
    result.credentialType = json["credential_type"].getStr()
  if json.hasKey("description"):
    result.description = json["description"].getStr()
  else:
    result.description = ""
  if json.hasKey("windmill_variable"):
    result.windmillVariable = json["windmill_variable"].getStr()
  if json.hasKey("created_at"):
    result.createdAt = json["created_at"].getStr()
  else:
    result.createdAt = ""
  if json.hasKey("last_used_at"):
    result.lastUsedAt = json["last_used_at"].getStr()
  else:
    result.lastUsedAt = ""

proc parseCredentialsFromJson*(json: JsonNode): seq[Credential] =
  result = @[]
  if json.kind == JArray:
    for item in json.items:
      result.add(parseCredentialFromJson(item))