import std/[json, strformat, strutils, re, logging]
import jester
import godon_windmill_adapter, types, config
import jsony

# Configure logging
addHandler(newConsoleLogger())
setLogFilter(lvlInfo)

proc isValidUUID*(uuid: string): bool =
  # Simple UUID v4 validation regex
  result = match(uuid, re(r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"))

proc createErrorResponse*(message: string, code: string, details: JsonNode = nil): JsonNode =
  result = %*{
    "message": message,
    "code": code
  }
  if details != nil:
    result["details"] = details

proc handleBreedersGet*(request: Request): (HttpCode, string) =
  try:
    info("GET /breeders request received")
    
    let cfg = loadConfig()
    var client = newWindmillClient(cfg)

    let breeders = client.getBreeders()

    info("Successfully retrieved " & $breeders.len & " breeders")
    result = (Http200, jsony.toJson(breeders))
  except ValueError as e:
    error("Validation error in GET /breeders: " & e.msg)
    let errorResponse = createErrorResponse("Invalid request parameters", "BAD_REQUEST", %*{"error": e.msg})
    result = (Http400, $errorResponse)
  except Exception as e:
    error("Internal error in GET /breeders: " & e.msg)
    let errorResponse = createErrorResponse("Failed to retrieve breeders", "INTERNAL_SERVER_ERROR", %*{"error": e.msg})
    result = (Http500, $errorResponse)

proc handleBreedersPost*(request: Request): (HttpCode, string) =
  try:
    info("POST /breeders request received")
    
    let requestBody = request.body
    if requestBody.len == 0:
      let errorResponse = createErrorResponse("Request body is required", "BAD_REQUEST")
      result = (Http400, $errorResponse)
      return
    
    let breederConfig = parseJson(requestBody)
    
    # Validate required fields
    if not breederConfig.hasKey("name"):
      let errorResponse = createErrorResponse("Missing required field: name", "BAD_REQUEST")
      result = (Http400, $errorResponse)
      return
    
    let cfg = loadConfig()
    var client = newWindmillClient(cfg)

    let response = client.createBreederResponse(breederConfig)

    info("Successfully created breeder")
    result = (Http201, jsony.toJson(response))
  except CatchableError:  # Jester uses CatchableError for JSON parsing
    error("Invalid JSON in POST /breeders: " & getCurrentExceptionMsg())
    let errorResponse = createErrorResponse("Invalid JSON in request body", "BAD_REQUEST")
    result = (Http400, $errorResponse)
  except Exception as e:
    error("Internal error in POST /breeders: " & e.msg)
    let errorResponse = createErrorResponse("Failed to create breeder", "INTERNAL_SERVER_ERROR", %*{"error": e.msg})
    result = (Http500, $errorResponse)

proc handleBreederGet*(request: Request, breederId: string): (HttpCode, string) =
  try:
    info("GET /breeders/" & breederId & " request received")
    
    # Validate UUID format
    if breederId.isEmptyOrWhitespace() or not isValidUUID(breederId):
      error("Invalid UUID format: " & breederId)
      let errorResponse = createErrorResponse("Invalid UUID format", "BAD_REQUEST", %*{"uuid": breederId})
      result = (Http400, $errorResponse)
      return
    
    let cfg = loadConfig()
    var client = newWindmillClient(cfg)

    let breeder = client.getBreeder(breederId)

    info("Successfully retrieved breeder: " & breederId)
    result = (Http200, jsony.toJson(breeder))
  except ValueError as e:
    error("Validation error in GET /breeders/" & breederId & ": " & e.msg)
    let errorResponse = createErrorResponse("Invalid request parameters", "BAD_REQUEST", %*{"error": e.msg})
    result = (Http400, $errorResponse)
  except Exception as e:
    error("Internal error in GET /breeders/" & breederId & ": " & e.msg)
    let errorResponse = createErrorResponse("Failed to retrieve breeder", "INTERNAL_SERVER_ERROR", %*{"error": e.msg})
    result = (Http500, $errorResponse)

proc handleBreederDelete*(request: Request, breederId: string): (HttpCode, string) =
  try:
    info("DELETE /breeders/" & breederId & " request received")

    # Validate UUID format
    if breederId.isEmptyOrWhitespace() or not isValidUUID(breederId):
      error("Invalid UUID format: " & breederId)
      let errorResponse = createErrorResponse("Invalid UUID format", "BAD_REQUEST", %*{"uuid": breederId})
      result = (Http400, $errorResponse)
      return

    # Parse force query parameter
    let forceParam = request.params.getOrDefault("force", "false")
    let force = forceParam == "true" or forceParam == "1"

    let cfg = loadConfig()
    var client = newWindmillClient(cfg)

    client.deleteBreeder(breederId, force)
    let response = %*{
      "id": breederId,
      "deleted": true,
      "force": force
    }

    info("Successfully deleted breeder: " & breederId & " (force=" & $force & ")")
    result = (Http200, $response)
  except ValueError as e:
    error("Validation error in DELETE /breeders/" & breederId & ": " & e.msg)
    let errorResponse = createErrorResponse("Invalid request parameters", "BAD_REQUEST", %*{"error": e.msg})
    result = (Http400, $errorResponse)
  except Exception as e:
    error("Internal error in DELETE /breeders/" & breederId & ": " & e.msg)
    let errorResponse = createErrorResponse("Failed to delete breeder", "INTERNAL_SERVER_ERROR", %*{"error": e.msg})
    result = (Http500, $errorResponse)

proc handleBreederStop*(request: Request, breederId: string): (HttpCode, string) =
  try:
    info("POST /breeders/" & breederId & "/stop request received")

    # Validate UUID format
    if breederId.isEmptyOrWhitespace() or not isValidUUID(breederId):
      error("Invalid UUID format: " & breederId)
      let errorResponse = createErrorResponse("Invalid UUID format", "BAD_REQUEST", %*{"uuid": breederId})
      result = (Http400, $errorResponse)
      return

    let cfg = loadConfig()
    var client = newWindmillClient(cfg)

    let response = client.stopBreeder(breederId)

    # Unwrap data field
    let data = if response.hasKey("data"): response["data"] else: response

    info("Successfully requested graceful shutdown for breeder: " & breederId)
    result = (Http200, $data)
  except ValueError as e:
    error("Validation error in POST /breeders/" & breederId & "/stop: " & e.msg)
    let errorResponse = createErrorResponse("Invalid request parameters", "BAD_REQUEST", %*{"error": e.msg})
    result = (Http400, $errorResponse)
  except Exception as e:
    error("Internal error in POST /breeders/" & breederId & "/stop: " & e.msg)
    let errorResponse = createErrorResponse("Failed to stop breeder", "INTERNAL_SERVER_ERROR", %*{"error": e.msg})
    result = (Http500, $errorResponse)

proc handleBreederStart*(request: Request, breederId: string): (HttpCode, string) =
  try:
    info("POST /breeders/" & breederId & "/start request received")

    # Validate UUID format
    if breederId.isEmptyOrWhitespace() or not isValidUUID(breederId):
      error("Invalid UUID format: " & breederId)
      let errorResponse = createErrorResponse("Invalid UUID format", "BAD_REQUEST", %*{"uuid": breederId})
      result = (Http400, $errorResponse)
      return

    let cfg = loadConfig()
    var client = newWindmillClient(cfg)

    let response = client.startBreeder(breederId)

    # Unwrap data field
    let data = if response.hasKey("data"): response["data"] else: response

    info("Successfully started breeder: " & breederId)
    result = (Http200, $data)
  except ValueError as e:
    error("Validation error in POST /breeders/" & breederId & "/start: " & e.msg)
    let errorResponse = createErrorResponse("Invalid request parameters", "BAD_REQUEST", %*{"error": e.msg})
    result = (Http400, $errorResponse)
  except Exception as e:
    error("Internal error in POST /breeders/" & breederId & "/start: " & e.msg)
    let errorResponse = createErrorResponse("Failed to start breeder", "INTERNAL_SERVER_ERROR", %*{"error": e.msg})
    result = (Http500, $errorResponse)

proc handleBreederPut*(request: Request, breederId: string): (HttpCode, string) =
  info("PUT /breeders/" & breederId & " request received")
  
  let errorResponse = createErrorResponse("Update breeder functionality not implemented", "NOT_IMPLEMENTED")
  result = (Http501, $errorResponse)

# Credential endpoints

proc handleCredentialsGet*(request: Request): (HttpCode, string) =
  try:
    info("GET /credentials request received")
    
    let cfg = loadConfig()
    var client = newWindmillClient(cfg)

    let credentials = client.getCredentials()

    info("Successfully retrieved " & $credentials.len & " credentials")
    result = (Http200, jsony.toJson(credentials))
  except Exception as e:
    error("Internal error in GET /credentials: " & e.msg)
    let errorResponse = createErrorResponse("Failed to retrieve credentials", "INTERNAL_SERVER_ERROR", %*{"error": e.msg})
    result = (Http500, $errorResponse)

proc handleCredentialsPost*(request: Request): (HttpCode, string) =
  try:
    info("POST /credentials request received")
    
    let requestBody = request.body
    if requestBody.len == 0:
      let errorResponse = createErrorResponse("Request body is required", "BAD_REQUEST")
      result = (Http400, $errorResponse)
      return
    
    let credentialData = parseJson(requestBody)
    
    # Validate required fields
    if not credentialData.hasKey("name") or not credentialData.hasKey("credentialType") or not credentialData.hasKey("content"):
      let errorResponse = createErrorResponse("Missing required fields: name, credentialType, content", "BAD_REQUEST")
      result = (Http400, $errorResponse)
      return
    
    let cfg = loadConfig()
    var client = newWindmillClient(cfg)
    
    # Step 1: Create Windmill variable with credential content
    let name = credentialData["name"].getStr()
    let content = credentialData["content"].getStr()
    let credentialType = credentialData["credentialType"].getStr()
    let windmillVariablePath = "f/vars/" & name

    # Validate name format (must be alphanumeric/hyphen/underscore only)
    if not name.match(re"^([a-zA-Z0-9_-]{1,})$"):
      error("Invalid name format: " & name)
      let errorResponse = createErrorResponse("Invalid name format: '" & name & "'. Use only alphanumeric characters, hyphens, and underscores", "BAD_REQUEST")
      result = (Http400, $errorResponse)
      return

    # Validate credential type
    let validTypes = @["ssh_private_key", "api_token", "database_connection", "http_basic_auth"]
    if credentialType notin validTypes:
      error("Invalid credential type: " & credentialType)
      let errorResponse = createErrorResponse("Invalid credentialType: '" & credentialType & "'. Must be one of: " & $validTypes, "BAD_REQUEST")
      result = (Http400, $errorResponse)
      return

    # Validate that content is not empty
    if content.strip() == "":
      error("Invalid content: content cannot be empty")
      let errorResponse = createErrorResponse("Invalid content: content cannot be empty", "BAD_REQUEST")
      result = (Http400, $errorResponse)
      return

    try:
      client.createVariable(windmillVariablePath, content, isSecret=true)
      info("Created Windmill variable: " & windmillVariablePath)
    except Exception as e:
      error("Failed to create Windmill variable: " & e.msg)
      # Check if it's a duplicate variable error (400 Bad Request from Windmill)
      let errorMsg = e.msg.toLowerAscii()
      if "already exists" in errorMsg or "400" in errorMsg:
        let errorResponse = createErrorResponse("Credential with name '" & name & "' already exists", "BAD_REQUEST")
        result = (Http400, $errorResponse)
      else:
        let errorResponse = createErrorResponse("Failed to create Windmill variable", "INTERNAL_SERVER_ERROR", %*{"error": e.msg})
        result = (Http500, $errorResponse)
      return
    
    # Step 2: Create catalog entry via controller script
    # Note: Content is stored in Windmill variable, not in controller catalog
    let catalogData = %*{
      "name": name,
      "credentialType": credentialType,
      "description": if credentialData.hasKey("description"): credentialData["description"].getStr() else: "",
      "windmillVariable": windmillVariablePath
    }
    
    let response = client.createCredentialResponse(catalogData)

    info("Successfully created credential")
    result = (Http201, jsony.toJson(response))
  except CatchableError:
    error("Invalid JSON in POST /credentials: " & getCurrentExceptionMsg())
    let errorResponse = createErrorResponse("Invalid JSON in request body", "BAD_REQUEST")
    result = (Http400, $errorResponse)
  except Exception as e:
    error("Internal error in POST /credentials: " & e.msg)
    let errorResponse = createErrorResponse("Failed to create credential", "INTERNAL_SERVER_ERROR", %*{"error": e.msg})
    result = (Http500, $errorResponse)

proc handleCredentialGet*(request: Request, credentialId: string): (HttpCode, string) =
  try:
    info("GET /credentials/" & credentialId & " request received")
    
    # Validate UUID format
    if credentialId.isEmptyOrWhitespace() or not isValidUUID(credentialId):
      error("Invalid UUID format: " & credentialId)
      let errorResponse = createErrorResponse("Invalid UUID format", "BAD_REQUEST", %*{"credential_id": credentialId})
      result = (Http400, $errorResponse)
      return
    
    let cfg = loadConfig()
    var client = newWindmillClient(cfg)
    
    # Step 1: Get catalog metadata from controller
    let credential = client.getCredential(credentialId)
    
    # Step 2: Get actual credential content from Windmill
    let windmillVariablePath = credential.windmillVariable
    let credentialContent = client.getVariable(windmillVariablePath)
    
    # Combine metadata + content in response
    let credentialWithContent = %*{
      "id": credential.id,
      "name": credential.name,
      "credentialType": credential.credentialType,
      "description": credential.description,
      "windmillVariable": credential.windmillVariable,
      "createdAt": credential.createdAt,
      "lastUsedAt": credential.lastUsedAt,
      "content": credentialContent
    }

    info("Successfully retrieved credential: " & credentialId)
    result = (Http200, $credentialWithContent)
  except ValueError as e:
    error("Validation error in GET /credentials/" & credentialId & ": " & e.msg)
    let errorResponse = createErrorResponse("Invalid request parameters", "BAD_REQUEST", %*{"error": e.msg})
    result = (Http400, $errorResponse)
  except Exception as e:
    error("Internal error in GET /credentials/" & credentialId & ": " & e.msg)
    let errorResponse = createErrorResponse("Failed to retrieve credential", "INTERNAL_SERVER_ERROR", %*{"error": e.msg})
    result = (Http500, $errorResponse)

proc handleCredentialDelete*(request: Request, credentialId: string): (HttpCode, string) =
  try:
    info("DELETE /credentials/" & credentialId & " request received")
    
    # Validate UUID format
    if credentialId.isEmptyOrWhitespace() or not isValidUUID(credentialId):
      error("Invalid UUID format: " & credentialId)
      let errorResponse = createErrorResponse("Invalid UUID format", "BAD_REQUEST", %*{"credential_id": credentialId})
      result = (Http400, $errorResponse)
      return
    
    let cfg = loadConfig()
    var client = newWindmillClient(cfg)
    
    # Step 1: Get credential info to find Windmill variable path
    let credential = client.getCredential(credentialId)
    let windmillVariablePath = credential.windmillVariable
    
    # Step 2: Delete from Windmill
    try:
      client.deleteVariable(windmillVariablePath)
      info("Deleted Windmill variable: " & windmillVariablePath)
    except Exception as e:
      error("Failed to delete Windmill variable: " & e.msg)
      # Continue anyway to clean up catalog
    
    # Step 3: Delete from catalog via controller script
    client.deleteCredentialResponse(credentialId)

    let response = %*{
      "id": credentialId,
      "deleted": true
    }

    info("Successfully deleted credential: " & credentialId)
    result = (Http200, $response)
  except ValueError as e:
    error("Validation error in DELETE /credentials/" & credentialId & ": " & e.msg)
    let errorResponse = createErrorResponse("Invalid request parameters", "BAD_REQUEST", %*{"error": e.msg})
    result = (Http400, $errorResponse)
  except Exception as e:
    error("Internal error in DELETE /credentials/" & credentialId & ": " & e.msg)
    let errorResponse = createErrorResponse("Failed to delete credential", "INTERNAL_SERVER_ERROR", %*{"error": e.msg})
    result = (Http500, $errorResponse)