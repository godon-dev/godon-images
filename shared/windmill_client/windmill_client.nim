import std/[httpclient, json, strformat, tables, uri, logging, strutils]
import config

type
  WindmillApiClient* = object
    config: WindmillConfig
    token: string
    http: HttpClient

proc login*(client: var WindmillApiClient) =
  ## Authenticate with Windmill and obtain bearer token
  let url = &"{client.config.windmillBaseUrl}/auth/login"
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
  
  # If windmillApiBaseUrl is already constructed, use it directly
  # Otherwise construct the full URL from components
  let url = if client.config.windmillApiBaseUrl != "":
              &"{client.config.windmillApiBaseUrl}/{jobPath}"
            else:
              let fullPath = "f/" & jobPath  
              let encodedPath = encodeUrl(fullPath)
              &"{client.config.windmillBaseUrl}/api/w/{client.config.windmillWorkspace}/jobs/run_wait_result/p/{encodedPath}"
  
  debug("Running job: " & jobPath)
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
  
  let url = &"{client.config.windmillBaseUrl}/workspaces/create"
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
    "language": language
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
  
  let url = &"{client.config.windmillBaseUrl}/w/{workspace}/flows/create"
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

proc deleteVariable*(client: WindmillApiClient, variablePath: string) =
  ## Delete a Windmill variable using the API
  info("Deleting variable: " & variablePath)
  
  # Parse variable path to extract components
  # Expected format: "f/vars/variable_name" or "vars/variable_name"
  let cleanPath = variablePath.replace("f/", "").replace("vars/", "")
  let encodedPath = encodeUrl(cleanPath)
  
  let url = &"{client.config.windmillBaseUrl}/api/w/{client.config.windmillWorkspace}/variables/{encodedPath}"
  
  try:
    let response = client.http.request(url, HttpDelete, "")
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
  
  # Parse variable path to extract components
  # Expected format: "f/vars/variable_name" or "vars/variable_name"
  let cleanPath = variablePath.replace("f/", "").replace("vars/", "")
  
  let url = &"{client.config.windmillBaseUrl}/api/w/{client.config.windmillWorkspace}/variables"
  let payload = %*{
    "path": cleanPath,
    "value": content,
    "isSecret": isSecret,
    "isPath": false  # We're storing file content as string value
  }
  
  try:
    let response = client.http.post(url, $payload)
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
  
  # Parse variable path to extract components
  let cleanPath = variablePath.replace("f/", "").replace("vars/", "")
  let encodedPath = encodeUrl(cleanPath)
  
  let url = &"{client.config.windmillBaseUrl}/api/w/{client.config.windmillWorkspace}/variables/value/{encodedPath}"
  
  try:
    let response = client.http.get(url)
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