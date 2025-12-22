import std/[httpclient, json, strformat, tables, uri, logging]
import config

type
  WindmillApiClient* = object
    config: WindmillConfig
    token: string
    http: HttpClient

proc login*(client: var WindmillApiClient) =
  ## Authenticate with Windmill and obtain bearer token
  let url = &"{client.config.windmillBaseUrl}/api/auth/login"
  let payload = %* {
    "email": client.config.windmillEmail,
    "password": client.config.windmillPassword
  }
  
  info("Logging into Windmill at: " & client.config.windmillBaseUrl)
  
  try:
    let response = client.http.post(url, $payload)
    if response.code != Http200:
      error("Windmill login failed: " & response.status)
      raise newException(ValueError, "Failed to login to Windmill: " & response.status)
    
    # Windmill returns the token as plaintext, not JSON
    client.token = response.body
    info("Successfully authenticated with Windmill")
  except CatchableError as e:
    error("Windmill authentication error: " & e.msg)
    raise

proc newWindmillApiClient*(config: WindmillConfig): WindmillApiClient =
  ## Create and authenticate a new Windmill client
  var http = newHttpClient()
  http.headers = newHttpHeaders({"Content-Type": "application/json"})
  
  result.config = config
  result.http = http
  result.login()
  
  # Set authorization header for future requests
  result.http.headers = newHttpHeaders({
    "Content-Type": "application/json",
    "Authorization": "Bearer " & result.token
  })

proc close*(client: WindmillApiClient) =
  ## Close the HTTP client
  client.http.close()

# API Methods - Pure Windmill API operations
proc runJob*(client: WindmillApiClient, jobPath: string, args: JsonNode = nil): JsonNode =
  ## Run a Windmill job (script or flow) by path and wait for result
  ## Uses the Windmill API URL pattern for job execution
  let fullPath = "f/" & jobPath
  let encodedPath = encodeUrl(fullPath)  # URL encode the path containing slashes
  let url = &"{client.config.windmillBaseUrl}/api/w/{client.config.windmillWorkspace}/jobs/run_wait_result/p/{encodedPath}"
  
  debug("Running job: " & fullPath)
  debug("URL: " & url)
  
  try:
    var body = ""
    if args != nil:
      body = $args
    else:
      body = "{}"
    
    let response = client.http.post(url, body)
    if response.code != Http200:
      error("Job execution failed: " & response.status)
      error("Response: " & response.body)
      raise newException(ValueError, "Windmill job execution failed: " & response.status)
    
    result = parseJson(response.body)
    debug("Job completed successfully")
  except CatchableError as e:
    error("Error running job " & jobPath & ": " & e.msg)
    raise

# Seeder-specific API methods
proc createWorkspace*(client: WindmillApiClient, workspace: string) =
  ## Create a new Windmill workspace using the API
  info("Creating workspace: " & workspace)
  
  let url = &"{client.config.windmillBaseUrl}/api/workspaces/create"
  let payload = %*{
    "id": workspace,
    "name": workspace,
    "username": client.config.windmillEmail
  }
  
  try:
    let response = client.http.post(url, $payload)
    if response.code == Http201:
      info("Successfully created workspace: " & workspace)
    elif response.code == Http409:
      info("Workspace already exists: " & workspace)
    else:
      error("Failed to create workspace: " & response.status)
      error("Response: " & response.body)
      raise newException(ValueError, "Failed to create workspace")
  except CatchableError as e:
    error("Error creating workspace: " & e.msg)
    raise

proc deployScript*(client: WindmillApiClient, workspace: string, scriptPath: string, content: string, settings: JsonNode = nil) =
  ## Deploy a script to Windmill using the API
  info("Deploying script: " & scriptPath)
  
  let url = &"{client.config.windmillBaseUrl}/api/w/{workspace}/scripts/create"
  var payload = %*{
    "path": scriptPath,
    "content": content
  }
  
  # Add script settings if provided
  if settings != nil:
    for key, value in settings.pairs:
      payload[key] = value
  
  try:
    let response = client.http.post(url, $payload)
    if response.code != Http201:
      error("Failed to deploy script: " & response.status)
      error("Response: " & response.body)
      raise newException(ValueError, "Failed to deploy script: " & scriptPath)
    info("Successfully deployed script: " & scriptPath)
  except CatchableError as e:
    error("Error deploying script: " & e.msg)
    raise

proc deployFlow*(client: WindmillApiClient, workspace: string, flowPath: string, flowDef: JsonNode) =
  ## Deploy a flow to Windmill using the API
  info("Deploying flow: " & flowPath)
  
  let url = &"{client.config.windmillBaseUrl}/api/w/{workspace}/flows/create"
  let payload = %*{
    "path": flowPath,
    "value": flowDef
  }
  
  try:
    let response = client.http.post(url, $payload)
    if response.code != Http201:
      error("Failed to deploy flow: " & response.status)
      error("Response: " & response.body)
      raise newException(ValueError, "Failed to deploy flow: " & flowPath)
    info("Successfully deployed flow: " & flowPath)
  except CatchableError as e:
    error("Error deploying flow: " & e.msg)
    raise