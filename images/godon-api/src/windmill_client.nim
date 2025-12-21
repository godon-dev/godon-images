import std/[json]
import config, types
import windmill_client

# Godon-specific wrapper around the shared WindmillClient
# This contains godon-specific business logic, keeping the shared client clean

# Godon-specific methods that use the shared client
proc getBreeders*(client: WindmillClient): seq[Breeder] =
  ## Execute the custom 'breeders_get' controller job
  let response = client.runJob("controller/breeders_get")
  if response.hasKey("breeders"):
    result = parseBreedersFromJson(response["breeders"])
  else:
    result = @[]

proc createBreeder*(client: WindmillClient, config: JsonNode): string =
  ## Execute the custom 'breeder_create' controller job
  let args = %* {"breeder_config": config}
  let response = client.runJob("controller/breeder_create", args)
  if response.hasKey("id"):
    result = response["id"].getStr()
  else:
    raise newException(ValueError, "No id returned from job")

proc createBreederResponse*(client: WindmillClient, config: JsonNode): JsonNode =
  ## Execute the custom 'breeder_create' controller job and return raw response
  let args = %* {"breeder_config": config}
  result = client.runJob("controller/breeder_create", args)

proc getBreeder*(client: WindmillClient, breederId: string): Breeder =
  ## Execute the custom 'breeder_get' controller job
  let args = %* {"breeder_id": breederId}
  let response = client.runJob("controller/breeder_get", args)
  result = parseBreederFromJson(response)

proc deleteBreeder*(client: WindmillClient, breederId: string) =
  ## Execute the custom 'breeder_delete' controller job
  let args = %* {"breeder_id": breederId}
  discard client.runJob("controller/breeder_delete", args)

# Create adapter to bridge godon-api Config to shared WindmillConfig
proc newWindmillClient*(config: Config): WindmillClient =
  ## Create a WindmillClient using godon-api's Config
  let windmillConfig = WindmillConfig(
    windmillBaseUrl: config.windmillBaseUrl,
    windmillApiBaseUrl: config.windmillApiBaseUrl,
    windmillWorkspace: config.windmillWorkspace,
    windmillEmail: "admin@windmill.dev",
    windmillPassword: "changeme"
  )
  result = newWindmillClient(windmillConfig)