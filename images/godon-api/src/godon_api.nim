import std/[strutils, os, json]
import jester

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
