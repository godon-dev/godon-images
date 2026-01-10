import std/[json, times]
import jsony

type
  BreederSummary* = object
    id*: string
    name*: string
    status*: string
    createdAt*: string

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
proc parseBreederSummaryFromJson*(json: JsonNode): BreederSummary =
  result = BreederSummary()
  if json.hasKey("id"):
    result.id = json["id"].getStr()
  if json.hasKey("name"):
    result.name = json["name"].getStr()
  if json.hasKey("status"):
    result.status = json["status"].getStr()
  if json.hasKey("createdAt"):
    result.createdAt = json["createdAt"].getStr()

proc parseBreederSummariesFromJson*(json: JsonNode): seq[BreederSummary] =
  result = @[]
  if json.kind == JArray:
    for item in json.items:
      result.add(parseBreederSummaryFromJson(item))

proc parseBreederFromJson*(json: JsonNode): Breeder =
  result = Breeder(config: newJObject())
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
  if json.hasKey("credentialType"):
    result.credentialType = json["credentialType"].getStr()
  if json.hasKey("description"):
    result.description = json["description"].getStr()
  else:
    result.description = ""
  if json.hasKey("windmillVariable"):
    result.windmillVariable = json["windmillVariable"].getStr()
  if json.hasKey("createdAt"):
    result.createdAt = json["createdAt"].getStr()
  else:
    result.createdAt = ""
  if json.hasKey("lastUsedAt"):
    result.lastUsedAt = json["lastUsedAt"].getStr()
  else:
    result.lastUsedAt = ""

proc parseCredentialsFromJson*(json: JsonNode): seq[Credential] =
  result = @[]
  if json.kind == JArray:
    for item in json.items:
      result.add(parseCredentialFromJson(item))