import std/[json]
import config, types

# Import WindmillConfig from shared client config  
# Note: The shared client is mounted at /shared/windmill_client
# We import modules individually to avoid conflicts
import windmill_client.config as sharedConfig
import windmill_client.windmill_client

# Export Windmill variable management methods from shared client
export windmill_client.createVariable, windmill_client.getVariable, windmill_client.deleteVariable

# Godon-specific wrapper around the shared WindmillApiClient
# This contains godon-specific business logic, keeping the shared client clean

# Godon-specific methods that use the shared client
proc getBreeders*(client: WindmillApiClient): seq[BreederSummary] =
  let response = client.runJob("breeders_get")
  if response.hasKey("breeders"):
    result = parseBreederSummariesFromJson(response["breeders"])
  else:
    result = @[]

proc createBreederResponse*(client: WindmillApiClient, breederConfig: JsonNode): JsonNode =
  let args = %* {"request_data": breederConfig}
  result = client.runJob("breeder_create", args)

proc getBreeder*(client: WindmillApiClient, breederId: string): Breeder =
  let args = %* {"request_data": {"breeder_id": breederId}}
  let response = client.runJob("breeder_get", args)
  result = parseBreederFromJson(response)

proc deleteBreeder*(client: WindmillApiClient, breederId: string) =
  let args = %* {"request_data": {"breeder_id": breederId}}
  discard client.runJob("breeder_delete", args)

# Credential management methods

proc getCredentials*(client: WindmillApiClient): seq[Credential] =
  let response = client.runJob("credentials_get")
  if response.kind == JArray:
    result = parseCredentialsFromJson(response)
  elif response.hasKey("credentials"):
    result = parseCredentialsFromJson(response["credentials"])
  else:
    result = @[]

proc createCredentialResponse*(client: WindmillApiClient, credentialData: JsonNode): JsonNode =
  let args = %* {"request_data": {"credential_data": credentialData}}
  result = client.runJob("credential_create", args)

proc getCredential*(client: WindmillApiClient, credentialId: string): Credential =
  let args = %* {"request_data": {"credential_id": credentialId}}
  let response = client.runJob("credential_get", args)
  if response.hasKey("credential"):
    result = parseCredentialFromJson(response["credential"])
  else:
    result = parseCredentialFromJson(response)

proc deleteCredentialResponse*(client: WindmillApiClient, credentialId: string): JsonNode =
  let args = %* {"request_data": {"credential_id": credentialId}}
  result = client.runJob("credential_delete", args)

# Create adapter to bridge godon-api Config to shared WindmillConfig
proc newWindmillClient*(godonCfg: Config): WindmillApiClient =
  let windmillConfig = sharedConfig.WindmillConfig(
    windmillBaseUrl: godonCfg.windmillBaseUrl,
    windmillApiBaseUrl: godonCfg.windmillApiBaseUrl,
    windmillWorkspace: godonCfg.windmillWorkspace,
    windmillEmail: "admin@windmill.dev",
    windmillPassword: "changeme",
    maxRetries: 3,  # API should fail fast rather than retry for extended periods
    retryDelay: 1    # Short delays for interactive API calls
  )
  result = newWindmillApiClient(windmillConfig)