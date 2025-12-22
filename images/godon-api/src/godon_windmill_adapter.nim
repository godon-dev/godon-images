import std/[json]
import config, types

# Import WindmillConfig from shared client config  
# Note: The shared client is mounted at /shared/windmill_client
# We import modules individually to avoid conflicts
import windmill_client.config as sharedConfig
import windmill_client.windmill_client

# Godon-specific wrapper around the shared WindmillApiClient
# This contains godon-specific business logic, keeping the shared client clean

# Godon-specific methods that use the shared client
proc getBreeders*(client: WindmillApiClient): seq[Breeder] =
  let response = client.runJob("breeders_get")
  if response.hasKey("breeders"):
    result = parseBreedersFromJson(response["breeders"])
  else:
    result = @[]

proc createBreeder*(client: WindmillApiClient, breederConfig: JsonNode): string =
  let args = %* {"breeder_config": breederConfig}
  let response = client.runJob("breeder_create", args)
  if response.hasKey("id"):
    result = response["id"].getStr()
  else:
    raise newException(ValueError, "No id returned from job")

proc createBreederResponse*(client: WindmillApiClient, breederConfig: JsonNode): JsonNode =
  let args = %* {"breeder_config": breederConfig}
  result = client.runJob("breeder_create", args)

proc getBreeder*(client: WindmillApiClient, breederId: string): Breeder =
  let args = %* {"breeder_id": breederId}
  let response = client.runJob("breeder_get", args)
  result = parseBreederFromJson(response)

proc deleteBreeder*(client: WindmillApiClient, breederId: string) =
  let args = %* {"breeder_id": breederId}
  discard client.runJob("breeder_delete", args)

# Create adapter to bridge godon-api Config to shared WindmillConfig
proc newWindmillClient*(godonCfg: Config): WindmillApiClient =
  let windmillConfig = sharedConfig.WindmillConfig(
    windmillBaseUrl: godonCfg.windmillBaseUrl,
    windmillApiBaseUrl: godonCfg.windmillApiBaseUrl,
    windmillWorkspace: godonCfg.windmillWorkspace,
    windmillEmail: "admin@windmill.dev",
    windmillPassword: "changeme"
  )
  result = newWindmillApiClient(windmillConfig)