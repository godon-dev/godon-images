import std/[json, strformat, strutils, re, logging]
import jester
import godon_windmill_adapter, types, config

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
    let response = %*breeders
    
    info("Successfully retrieved " & $breeders.len & " breeders")
    result = (Http200, $response)
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
    result = (Http201, $response)
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
    let response = %*breeder
    
    info("Successfully retrieved breeder: " & breederId)
    result = (Http200, $response)
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
    
    let cfg = loadConfig()
    var client = newWindmillClient(cfg)
    
    client.deleteBreeder(breederId)
    let response = %*{
      "message": "Purged Breeder " & breederId
    }
    
    info("Successfully deleted breeder: " & breederId)
    result = (Http200, $response)
  except ValueError as e:
    error("Validation error in DELETE /breeders/" & breederId & ": " & e.msg)
    let errorResponse = createErrorResponse("Invalid request parameters", "BAD_REQUEST", %*{"error": e.msg})
    result = (Http400, $errorResponse)
  except Exception as e:
    error("Internal error in DELETE /breeders/" & breederId & ": " & e.msg)
    let errorResponse = createErrorResponse("Failed to delete breeder", "INTERNAL_SERVER_ERROR", %*{"error": e.msg})
    result = (Http500, $errorResponse)

proc handleBreederPut*(request: Request, breederId: string): (HttpCode, string) =
  info("PUT /breeders/" & breederId & " request received")
  
  let errorResponse = createErrorResponse("Update breeder functionality not implemented", "NOT_IMPLEMENTED")
  result = (Http501, $errorResponse)