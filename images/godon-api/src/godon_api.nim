import std/[strutils, os, json]
import jester
import handlers

settings:
  port = Port(parseInt(getEnv("PORT", "8080")))
  bindAddr = "0.0.0.0"
  # Enable proper threading with Nim 2.0 compatibility fix
  reusePort = true

routes:
  options "/@path*":
    resp Http204, [(
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], ""

  get "/health":
    resp Http200, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], $(%*{"status": "healthy", "service": "godon-api", "version": "0.1.0"})

  get "/":
    resp Http200, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], $(%*{"message": "Godon API is running", "version": "0.1.0"})

  # Breeder endpoints
  get "/breeders":
    let (code, response) = handleBreedersGet(request)
    resp code, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], response

  post "/breeders":
    let (code, response) = handleBreedersPost(request)
    resp code, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], response

  get "/breeders/@id":
    let (code, response) = handleBreederGet(request, @"id")
    resp code, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], response

  put "/breeders/@id":
    let (code, response) = handleBreederPut(request, @"id")
    resp code, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], response

  delete "/breeders/@id":
    let (code, response) = handleBreederDelete(request, @"id")
    resp code, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], response

  # Credential endpoints
  get "/credentials":
    let (code, response) = handleCredentialsGet(request)
    resp code, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], response

  post "/credentials":
    let (code, response) = handleCredentialsPost(request)
    resp code, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], response

  get "/credentials/@id":
    let (code, response) = handleCredentialGet(request, @"id")
    resp code, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], response

  delete "/credentials/@id":
    let (code, response) = handleCredentialDelete(request, @"id")
    resp code, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], response

  error Http404:
    let errorResponse = %*{"message": "Not found", "code": "NOT_FOUND"}
    resp Http404, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], $errorResponse

when isMainModule:
  import std/[logging]
  
  # Configure logging
  addHandler(newConsoleLogger())
  setLogFilter(lvlInfo)
  
  info("Starting Godon API server with Jester (Nim 2.0 compatibility)")
