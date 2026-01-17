import std/[httpclient, json, strformat, tables, uri, logging, strutils, os]
import config

type
  WindmillApiClient* = object
    config: WindmillConfig
    token: string
    http: HttpClient

proc login*(client: var WindmillApiClient) =
  ## Authenticate with Windmill and obtain bearer token
  ## Includes configurable retry logic for connection failures
  let url = &"{client.config.windmillBaseUrl}/auth/login"
  let payload = %* {
    "email": client.config.windmillEmail,
    "password": client.config.windmillPassword
  }

  info("Logging into Windmill at: " & client.config.windmillBaseUrl)

  # Use retry configuration from WindmillConfig
  let maxRetries = client.config.maxRetries
  let retryDelay = client.config.retryDelay

  var retries = 0
  while retries < maxRetries:
    try:
      let originalHeaders = client.http.headers
      client.http.headers = newHttpHeaders({"Content-Type": "application/json"})
      let response = client.http.post(url, $payload)
      # Clear headers completely after login, will be set properly below
      client.http.headers = newHttpHeaders()
      if response.code != Http200:
        error("Windmill login failed: " & response.status)
        error("Response body: " & response.body)
        raise newException(ValueError, "Failed to login to Windmill: " & response.status)

      # Windmill returns the token as plaintext, not JSON
      debug("Login response code: " & $response.code)
      debug("Login response body (first 50 chars): " & response.body[0..min(49, response.body.len-1)])
      client.token = response.body
      info("Successfully authenticated with Windmill")

      # Set authorization header for future requests (not Content-Type - set per-request)
      client.http.headers = newHttpHeaders({
        "Authorization": "Bearer " & client.token
      })
      return  # Success, exit retry loop
    except Exception as e:
      inc retries
      if retries >= maxRetries:
        error("Windmill authentication error after $1 attempts: $2" % [$maxRetries, e.msg])
        raise
      else:
        warn("Connection attempt $1 failed: $2. Retrying in $3 seconds..." % [$retries, e.msg, $retryDelay])
        sleep(retryDelay * 1000)

proc newWindmillApiClient*(config: WindmillConfig): WindmillApiClient =
  ## Create and authenticate a new Windmill client
  var http = newHttpClient()
  # Don't set Content-Type globally - only for POST/PUT requests
  
  result.config = config
  result.http = http
  result.login()

proc close*(client: WindmillApiClient) =
  ## Close the HTTP client
  client.http.close()

# API Methods - Pure Windmill API operations
proc runJob*(client: WindmillApiClient, jobPath: string, args: JsonNode = nil): JsonNode =
  ## Run a Windmill job (script or flow) by path and wait for result
  ## Uses the Windmill API URL pattern for job execution

  # If windmillApiBaseUrl is already constructed, use it directly
  # Otherwise construct the full URL from components
  # Note: windmillBaseUrl may include /api - don't duplicate it
  let url = if client.config.windmillApiBaseUrl != "":
              &"{client.config.windmillApiBaseUrl}/{jobPath}"
            else:
              let fullPath = "f/" & jobPath
              let encodedPath = encodeUrl(fullPath)
              # Use baseUrl directly (may or may not include /api)
              &"{client.config.windmillBaseUrl}/w/{client.config.windmillWorkspace}/jobs/run_wait_result/p/{encodedPath}"
  
  debug("Running job: " & jobPath)
  debug("URL: " & url)
  
  try:
    var body = ""
    if args != nil:
      body = $args
    else:
      body = "{}"
    
    let originalHeaders = client.http.headers
    client.http.headers = newHttpHeaders({
      "Content-Type": "application/json",
      "Authorization": "Bearer " & client.token
    })
    let response = client.http.post(url, body)
    # Reset headers to only Authorization after POST
    client.http.headers = newHttpHeaders({
      "Authorization": "Bearer " & client.token
    })
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
proc workspaceExists*(client: WindmillApiClient, workspace: string): bool =
  ## Check if a workspace exists using the API
  debug("Checking if workspace exists: " & workspace)
  
  let url = &"{client.config.windmillBaseUrl}/workspaces/exists"
  let payload = %*{
    "id": workspace
  }
  
  try:
    let originalHeaders = client.http.headers
    client.http.headers = newHttpHeaders({
      "Content-Type": "application/json",
      "Authorization": "Bearer " & client.token
    })
    let response = client.http.post(url, $payload)
    # Reset headers to only Authorization after POST
    client.http.headers = newHttpHeaders({
      "Authorization": "Bearer " & client.token
    })
    
    # Response body should contain "true" or "false"
    let exists = response.body.strip().toLower() == "true"
    debug("Workspace " & workspace & " exists: " & $exists)
    return exists
  except CatchableError as e:
    error("Error checking workspace existence: " & e.msg)
    # Assume workspace doesn't exist if check fails
    return false

proc createWorkspace*(client: WindmillApiClient, workspace: string) =
  ## Create a new Windmill workspace using the API (idempotent - checks if exists first)
  
  # Check if workspace already exists
  if client.workspaceExists(workspace):
    info("Workspace already exists: " & workspace)
    return
  
  info("Creating workspace: " & workspace)

  let url = &"{client.config.windmillBaseUrl}/workspaces/create"
  let payload = %* {
    "id": workspace,
    "name": workspace
  }
  
  debug("Creating workspace at URL: " & url)
  debug("Payload: " & $payload)
  
  try:
    let originalHeaders = client.http.headers
    client.http.headers = newHttpHeaders({
      "Content-Type": "application/json",
      "Authorization": "Bearer " & client.token
    })
    debug("Sending request with token: " & client.token[0..10] & "...")
    let response = client.http.post(url, $payload)
    debug("Response code: " & $response.code & " - " & response.status)
    debug("Response body: " & response.body)
    # Reset headers to only Authorization after POST
    client.http.headers = newHttpHeaders({
      "Authorization": "Bearer " & client.token
    })
    if response.code == Http201 or response.code == Http200:
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

proc createFolder*(client: WindmillApiClient, workspace: string, folderPath: string) =
  ## Create a folder in Windmill using the API
  info("Creating folder: " & folderPath)
  
  # Extract folder name from path (remove f/ prefix if present)
  let folderName = if folderPath.startsWith("f/"):
                     folderPath[2..folderPath.len-1]  # Remove "f/" prefix
                   else:
                     folderPath
  
  let url = &"{client.config.windmillBaseUrl}/w/{workspace}/folders/create"
  let payload = %*{
    "name": folderName
  }
  
  debug("Creating folder at URL: " & url)
  debug("Payload: " & $payload)
  
  try:
    let originalHeaders = client.http.headers
    client.http.headers = newHttpHeaders({
      "Content-Type": "application/json",
      "Authorization": "Bearer " & client.token
    })
    debug("Sending folder creation request with token: " & client.token[0..10] & "...")
    let response = client.http.post(url, $payload)
    debug("Folder creation response code: " & $response.code & " - " & response.status)
    debug("Folder creation response body: " & response.body)
    # Reset headers to only Authorization after POST
    client.http.headers = newHttpHeaders({
      "Authorization": "Bearer " & client.token
    })
    
    if response.code == Http200:
      info("Successfully created folder: " & folderPath)
    elif response.code == Http409 or response.code == Http400:
      info("Folder already exists: " & folderPath)
    else:
      error("Failed to create folder: " & response.status)
      error("Response: " & response.body)
      raise newException(ValueError, "Failed to create folder")
  except CatchableError as e:
    error("Error creating folder: " & e.msg)
    raise

proc existsScript*(client: WindmillApiClient, workspace: string, scriptPath: string): bool =
  ## Check if a script exists in Windmill
  let url = &"{client.config.windmillBaseUrl}/w/{workspace}/scripts/exists/p/{scriptPath}"

  try:
    let response = client.http.get(url)
    if response.code == Http200:
      let exists = parseJson(response.body).getBool()
      return exists
    else:
      error("Failed to check script existence: " & response.status)
      return false
  except CatchableError as e:
    error("Error checking script existence: " & e.msg)
    return false

proc existsFlow*(client: WindmillApiClient, workspace: string, flowPath: string): bool =
  ## Check if a flow exists in Windmill (flows have separate endpoint)
  let url = &"{client.config.windmillBaseUrl}/w/{workspace}/flows/exists/{flowPath}"

  try:
    let response = client.http.get(url)
    if response.code == Http200:
      let exists = parseJson(response.body).getBool()
      return exists
    else:
      error("Failed to check flow existence: " & response.status)
      return false
  except CatchableError as e:
    error("Error checking flow existence: " & e.msg)
    return false

proc deployScript*(client: WindmillApiClient, workspace: string, scriptPath: string, content: string, settings: JsonNode = nil) =
  ## Deploy a script to Windmill using the API
  info("Deploying script: " & scriptPath)
  
  # Detect language from file extension
  let ext = if '.' in scriptPath:
               let parts = scriptPath.split('.')
               "." & parts[parts.len - 1]
             else:
               ""
  let language = case ext
    of ".py": "python3"
    of ".js": "deno"
    of ".go": "go"
    of ".sh": "bash"
    of ".sql": "postgresql"
    of ".ts": "nativets"
    else: "python3"  # Default fallback
  
  let url = &"{client.config.windmillBaseUrl}/w/{workspace}/scripts/create"
  var payload = %*{
    "path": scriptPath,
    "content": content,
    "language": language,
    "summary": "",
    "description": ""
  }

  # Add script settings if provided
  if settings != nil:
    for key, value in settings.pairs:
      payload[key] = value
  
  try:
    let originalHeaders = client.http.headers
    client.http.headers = newHttpHeaders({
      "Content-Type": "application/json",
      "Authorization": "Bearer " & client.token
    })
    let response = client.http.post(url, $payload)
    # Reset headers to only Authorization after POST
    client.http.headers = newHttpHeaders({
      "Authorization": "Bearer " & client.token
    })
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

  let url = &"{client.config.windmillBaseUrl}/w/{workspace}/flows/create"
  let payload = flowDef
  
  try:
    let originalHeaders = client.http.headers
    client.http.headers = newHttpHeaders({
      "Content-Type": "application/json",
      "Authorization": "Bearer " & client.token
    })
    let response = client.http.post(url, $payload)
    # Reset headers to only Authorization after POST
    client.http.headers = newHttpHeaders({
      "Authorization": "Bearer " & client.token
    })
    if response.code != Http201:
      error("Failed to deploy flow: " & response.status)
      error("Response: " & response.body)
      raise newException(ValueError, "Failed to deploy flow: " & flowPath)
    info("Successfully deployed flow: " & flowPath)
  except CatchableError as e:
    error("Error deploying flow: " & e.msg)
    raise

proc deleteVariable*(client: WindmillApiClient, variablePath: string) =
  ## Delete a Windmill variable using the API
  info("Deleting variable: " & variablePath)

  # Ensure path starts with f/ if it doesn't have a prefix
  let fullPath = if variablePath.startsWith("f/") or variablePath.startsWith("u/") or variablePath.startsWith("g/"):
    variablePath
  else:
    "f/" & variablePath

  let encodedPath = encodeUrl(fullPath)

  let url = &"{client.config.windmillBaseUrl}/w/{client.config.windmillWorkspace}/variables/delete/{encodedPath}"
  
  try:
    # Create a fresh HTTP client to avoid any state pollution from POST operations
    var freshClient = newHttpClient()
    freshClient.headers = newHttpHeaders({
      "Authorization": "Bearer " & client.token
    })
    let response = freshClient.request(url, HttpDelete, "")
    if response.code == Http200:
      info("Successfully deleted variable: " & variablePath)
    elif response.code == Http404:
      info("Variable not found (may have been already deleted): " & variablePath)
    else:
      error("Failed to delete variable: " & response.status)
      error("Response: " & response.body)
      raise newException(ValueError, "Failed to delete variable: " & variablePath)
  except CatchableError as e:
    error("Error deleting variable: " & e.msg)
    raise

proc createVariable*(client: WindmillApiClient, variablePath: string, content: string, isSecret: bool = true) =
  ## Create a Windmill variable using the API
  info("Creating variable: " & variablePath)

  # Ensure path starts with f/ if it doesn't have a prefix
  let fullPath = if variablePath.startsWith("f/") or variablePath.startsWith("u/") or variablePath.startsWith("g/"):
    variablePath
  else:
    "f/" & variablePath

  let url = &"{client.config.windmillBaseUrl}/w/{client.config.windmillWorkspace}/variables/create"
  let payload = %*{
    "path": fullPath,
    "value": content,
    "is_secret": isSecret,
    "description": "",  # Required by Windmill API
    "isPath": false  # We're storing file content as string value
  }
  
  try:
    let originalHeaders = client.http.headers
    client.http.headers = newHttpHeaders({
      "Content-Type": "application/json",
      "Authorization": "Bearer " & client.token
    })
    let response = client.http.post(url, $payload)
    # Reset headers to only Authorization after POST
    client.http.headers = newHttpHeaders({
      "Authorization": "Bearer " & client.token
    })
    if response.code == Http201:
      info("Successfully created variable: " & variablePath)
    elif response.code == Http409:
      info("Variable already exists: " & variablePath)
    else:
      error("Failed to create variable: " & response.status)
      error("Response: " & response.body)
      raise newException(ValueError, "Failed to create variable: " & variablePath)
  except CatchableError as e:
    error("Error creating variable: " & e.msg)
    raise

proc getVariable*(client: WindmillApiClient, variablePath: string): string =
  ## Get a Windmill variable content by path
  info("Getting variable: " & variablePath)

  # Ensure path starts with f/ if it doesn't have a prefix
  let fullPath = if variablePath.startsWith("f/") or variablePath.startsWith("u/") or variablePath.startsWith("g/"):
    variablePath
  else:
    "f/" & variablePath

  let encodedPath = encodeUrl(fullPath)

  let url = &"{client.config.windmillBaseUrl}/w/{client.config.windmillWorkspace}/variables/get_value/{encodedPath}"
  
  try:
    # Create a fresh HTTP client to avoid any state pollution from POST operations
    var freshClient = newHttpClient()
    freshClient.headers = newHttpHeaders({
      "Authorization": "Bearer " & client.token
    })
    let response = freshClient.get(url)
    if response.code == Http200:
      info("Successfully retrieved variable: " & variablePath)
      result = response.body
    elif response.code == Http404:
      error("Variable not found: " & variablePath)
      raise newException(ValueError, "Variable not found: " & variablePath)
    else:
      error("Failed to get variable: " & response.status)
      error("Response: " & response.body)
      raise newException(ValueError, "Failed to get variable: " & variablePath)
  except CatchableError as e:
    error("Error getting variable: " & e.msg)
    raise