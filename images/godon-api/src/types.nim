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