import std/[strutils, os, json]
import jester
import handlers

settings:
  port = Port(parseInt(getEnv("PORT", "8080")))
  bindAddr = "0.0.0.0"

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

  get "/v0/breeders":
    let (code, body) = handleBreedersGet(request)
    resp code, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], body

  post "/v0/breeders":
    let (code, body) = handleBreedersPost(request)
    resp code, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], body

  get "/v0/breeders/@breederId":
    let (code, body) = handleBreederGet(request, @"breederId")
    resp code, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], body

  delete "/v0/breeders/@breederId":
    let (code, body) = handleBreederDelete(request, @"breederId")
    resp code, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], body

  put "/v0/breeders/@breederId":
    let (code, body) = handleBreederPut(request, @"breederId")
    resp code, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], body

  get "/breeders":
    let (code, body) = handleBreedersGet(request)
    resp code, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], body

  post "/breeders":
    let (code, body) = handleBreedersPost(request)
    resp code, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], body

  get "/breeders/@breederId":
    let (code, body) = handleBreederGet(request, @"breederId")
    resp code, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], body

  delete "/breeders/@breederId":
    let (code, body) = handleBreederDelete(request, @"breederId")
    resp code, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], body

  put "/breeders/@breederId":
    let (code, body) = handleBreederPut(request, @"breederId")
    resp code, [(
      key: "Content-Type", 
      val: "application/json"
    ), (
      key: "Access-Control-Allow-Origin",
      val: "*"
    )], body

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
  
  info("Starting Godon API server")
