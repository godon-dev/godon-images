import std/[strutils, os, json]
import jester
import handlers


# Helper to handle responses with proper headers
proc respond*(statusCode: HttpCode, body: string): Response =
  result = Response(
    code: statusCode,
    body: body,
    headers: newHttpHeaders([
      ("Content-Type", "application/json"),
      ("Access-Control-Allow-Origin", "*")
    ])
  )

# Routes
routes:
  # Handle CORS preflight
  options "/@path*":
    respond(Http204, "")

  # Health check
  get "/health":
    respond(Http200, $(%*{"status": "healthy", "service": "godon-api", "version": "0.1.0"}))

  # API version 0 (for OpenAPI compatibility)
  # Breeders endpoints
  get "/v0/breeders":
    let (code, body) = handleBreedersGet(request)
    respond(code, body)

  post "/v0/breeders":
    let (code, body) = handleBreedersPost(request)
    respond(code, body)

  get "/v0/breeders/@breederId":
    let (code, body) = handleBreederGet(request, @"breederId")
    respond(code, body)

  delete "/v0/breeders/@breederId":
    let (code, body) = handleBreederDelete(request, @"breederId")
    respond(code, body)

  put "/v0/breeders/@breederId":
    let (code, body) = handleBreederPut(request, @"breederId)
    respond(code, body)

  # Root breeders endpoints
  get "/breeders":
    let (code, body) = handleBreedersGet(request)
    respond(code, body)

  post "/breeders":
    let (code, body) = handleBreedersPost(request)
    respond(code, body)

  get "/breeders/@breederId":
    let (code, body) = handleBreederGet(request, @"breederId")
    respond(code, body)

  delete "/breeders/@breederId":
    let (code, body) = handleBreederDelete(request, @"breederId")
    respond(code, body)

  put "/breeders/@breederId":
    let (code, body) = handleBreederPut(request, @"breederId)
    respond(code, body)

  # 404 for unknown routes
  error Http404:
    let errorResponse = %*{"message": "Not found", "code": "NOT_FOUND"}
    respond(Http404, $errorResponse)

when isMainModule:
  let port = Port(parseInt(getEnv("PORT", "8080")))
  let serverSettings = newSettings(
    port = port,
    bindAddr = "0.0.0.0"
  )
  
  echo "🚀 Starting Godon API server on port " & $int(port)
  echo "📊 Service Configuration:"
  echo "  - Memory Management: Reference Counting (refc)"
  echo "  - Bind Address: 0.0.0.0"
  echo "  - Windmill Workspace: " & getEnv("WINDMILL_WORKSPACE", "godon")
  echo "  - Windmill Host: " & getEnv("WINDMILL_HOST", "localhost")
  echo ""
  echo "🔗 Available Endpoints:"
  echo "  GET    /health                 - Health check"
  echo "  GET    /breeders               - List all breeders"
  echo "  POST   /breeders               - Create new breeder"
  echo "  GET    /breeders/{uuid}        - Get specific breeder"
  echo "  DELETE /breeders/{uuid}        - Delete breeder"
  echo "  PUT    /breeders/{uuid}        - Update breeder (501 - Not Implemented)"
  echo ""
  echo "📝 All endpoints also available under /v0/ prefix for OpenAPI compatibility"
  echo ""
  
  run(serverSettings)