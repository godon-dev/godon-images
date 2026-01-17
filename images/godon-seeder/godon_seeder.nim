import std/[parseopt, strutils, os, logging, json, times, tables, sequtils, sets, options, streams]
import std/jsonutils
import yaml
import yaml/tojson
import windmill_client.config
import windmill_client.windmill_client

addHandler(newConsoleLogger())
setLogFilter(lvlInfo)

type
  # Component configuration types
  ScriptSettings* {.sparse.} = object
    summary*: string
    description*: string
    timeout*: Option[int]
    tag*: Option[string]
    # Any other Windmill script settings can be added in component.yaml
    # and will be passed through to Windmill API

  FlowSettings* {.sparse.} = object
    summary*: string
    description*: string
    deployment_message*: Option[string]

  ScriptSpec* {.sparse.} = object
    pattern*: string
    path*: Option[string]    # Optional: for subdirectory specification
    settings*: ScriptSettings

  FlowSpec* {.sparse.} = object
    pattern*: string
    path*: Option[string]    # Optional: for subdirectory specification
    settings*: FlowSettings

  ComponentConfig* {.sparse.} = object
    name*: string
    target*: string
    workspace*: string
    scripts*: seq[ScriptSpec]
    flows*: Option[seq[FlowSpec]]

  ComponentInfo* = object
    config*: ComponentConfig
    directory*: string

  # Main seeder configuration
  SeederConfig* = object
    windmillBaseUrl*: string
    windmillWorkspace*: string
    windmillEmail*: string
    windmillPassword*: string
    godonVersion*: string
    godonDir*: string
    sourceDirs*: seq[string]
    verbose*: bool
    maxRetries*: int
    retryDelay*: int

proc parseScriptSettings(settingsJson: JsonNode): ScriptSettings =
  ## Parse script settings from JSON node
  ## With sparse objects, YAML parser automatically includes all fields
  ## We just validate required string fields have defaults
  result = to(settingsJson, ScriptSettings)
  if result.summary.len == 0:
    result.summary = ""
  if result.description.len == 0:
    result.description = ""

proc parseScriptSpec(specJson: JsonNode): ScriptSpec =
  ## Parse script specification from JSON node
  let settingsJson = specJson.getOrDefault("settings")
  let pathValue = specJson.getOrDefault("path")
  result = ScriptSpec(
    pattern: specJson.getOrDefault("pattern").getStr(""),
    path: if pathValue.kind != JNull and pathValue.len > 0: some(pathValue.getStr()) else: none(string),
    settings: if settingsJson != nil and settingsJson.kind == JObject: parseScriptSettings(settingsJson) else: ScriptSettings()
  )

proc parseComponentConfig(yamlPath: string): ComponentConfig =
  ## Parse component.yaml file into ComponentConfig object
  info("Parsing component config: " & yamlPath)

  let yamlContent = readFile(yamlPath)

  # Parse YAML to JSON first, then convert to ComponentConfig
  try:
    let yamlData = loadToJson(yamlContent)
    if yamlData.len == 0:
      raise newException(IOError, "Empty YAML file")

    let json = yamlData[0]

    # Use to() which respects sparse objects - allows extra fields in YAML
    result = to(json, ComponentConfig)

    info("Parsed component '" & result.name & "' with " & $result.scripts.len & " scripts")
  except CatchableError as e:
    logging.error("Failed to parse YAML: " & e.msg)
    # Return empty config as fallback
    result = ComponentConfig(
      name: yamlPath.splitPath().head,
      target: "",
      workspace: "",
      scripts: @[]
    )

proc loadSeederConfig*: SeederConfig =
  ## Load configuration from environment variables
  result.windmillBaseUrl = getEnv("WINDMILL_BASE_URL", "http://localhost:8000")
  result.windmillWorkspace = getEnv("WINDMILL_WORKSPACE", "godon")
  result.windmillEmail = getEnv("WINDMILL_EMAIL", "admin@windmill.dev")
  result.windmillPassword = getEnv("WINDMILL_PASSWORD", "changeme")
  result.godonVersion = getEnv("GODON_VERSION", "main")
  result.godonDir = getEnv("GODON_DIR", "/godon")
  result.verbose = getEnv("VERBOSE", "false") == "true"
  result.maxRetries = parseInt(getEnv("SEEDER_MAX_RETRIES", "30"))
  result.retryDelay = parseInt(getEnv("SEEDER_RETRY_DELAY", "2"))
  result.sourceDirs = @[]

proc discoverComponents*(config: SeederConfig): seq[ComponentInfo] =
  ## Discover component configurations from source directories
  result = @[]

  for sourceDir in config.sourceDirs:
    if not dirExists(sourceDir):
      warn("Source directory does not exist: " & sourceDir)
      continue

    info("Scanning directory: " & sourceDir)

    # Look for component.yaml files recursively
    for componentPath in walkDirRec(sourceDir):
      if componentPath.endsWith("component.yaml"):
        # walkDirRec returns absolute paths, use directly
        info("Found component config: " & componentPath)
        try:
          let component = parseComponentConfig(componentPath)
          # Extract the actual component directory (where component.yaml is located)
          let componentDir = componentPath.splitPath().head
          # Create ComponentInfo with the correct component directory
          let componentInfo = ComponentInfo(
            config: component,
            directory: componentDir
          )
          result.add(componentInfo)
        except CatchableError as e:
          logging.error("Failed to parse component config " & componentPath & ": " & e.msg)

proc findFilesByPattern(baseDir: string, pattern: string): seq[string] =
  ## Find files matching a glob pattern relative to base directory
  result = @[]

  let patternPath = baseDir / pattern
  let (dir, filePattern) = patternPath.splitPath()

  if not dirExists(dir):
    warn("Pattern directory does not exist: " & dir)
    return result

  debug("Searching for files matching pattern: " & pattern & " in " & dir)

  for kind, path in walkDir(dir, relative=true):
    if kind == pcFile:
      # Simple glob matching - check if filename matches pattern
      let filename = path.extractFilename()
      let ext = "." & filename.split('.')[^1]

      # Match extension or exact pattern
      if filePattern == "*" or
         filePattern == ext or
         filePattern == ("*" & ext) or
         filename == filePattern:
        let fullPath = dir / path
        result.add(fullPath)
        debug("  Found matching file: " & fullPath)

proc readFlowContent(flowPath: string): string =
  ## Read flow YAML content as raw string
  if not fileExists(flowPath):
    raise newException(IOError, "Flow file not found: " & flowPath)

  result = readFile(flowPath)
  debug("Read flow content from: " & flowPath & " (" & $result.len & " bytes)")

proc readScriptContent(scriptPath: string): string =
  ## Read script content and determine language from extension
  if not fileExists(scriptPath):
    raise newException(IOError, "Script file not found: " & scriptPath)

  result = readFile(scriptPath)
  debug("Read script content from: " & scriptPath & " (" & $result.len & " bytes)")

proc deployFlow*(client: WindmillApiClient, workspace: string, flowPath: string, flowYaml: string, settings: FlowSettings) =
  ## Deploy a single flow to Windmill using YAML content (following Windmill CLI pattern)
  info("Deploying flow: " & flowPath)

  # Parse YAML into JsonNode using NimYAML's loadToJson function
  var flowJson: JsonNode
  try:
    # loadToJson returns a sequence of documents, take the first one
    let docs = loadToJson(flowYaml)
    if docs.len > 0:
      flowJson = docs[0]
    else:
      raise newException(IOError, "No YAML documents found")
  except CatchableError as e:
    raise newException(IOError, "Failed to parse flow YAML: " & e.msg)
  
  # Override summary and description from settings (if provided)
  # Windmill 1.601.1+ requires these fields to be present
  if settings.summary.len > 0:
    flowJson["summary"] = %* settings.summary
  else:
    flowJson["summary"] = %* ""

  if settings.description.len > 0:
    flowJson["description"] = %* settings.description
  else:
    flowJson["description"] = %* ""
  
  # Build the complete API request by adding API-specific fields
  var requestPayload = newJObject()
  requestPayload["path"] = %* flowPath
  
  # Spread the flow definition (like Windmill CLI does with ...localFlow)
  for key, value in pairs(flowJson):
    requestPayload[key] = value
  
  # Add deployment message if provided
  if settings.deployment_message.isSome:
    requestPayload["deployment_message"] = %* settings.deployment_message.get()

  debug("Flow request payload: " & $requestPayload)
  debug("Flow JSON structure: " & requestPayload.pretty)
  # Pass the complete flow JSON directly (the flow YAML already has the correct structure)
  client.deployFlow(workspace, flowPath, requestPayload)
  info("✅ Successfully deployed flow: " & flowPath)

proc deployScript*(client: WindmillApiClient, workspace: string, scriptPath: string, content: string, settings: ScriptSettings) =
  ## Deploy a single script to Windmill
  info("Deploying script: " & scriptPath)

  # Convert ScriptSettings object to JSON to pass through all fields
  var scriptSettings = %(settings)
  # Ensure required fields are present (Windmill 1.601.1+ requires these)
  if scriptSettings.hasKey("summary") and scriptSettings["summary"].kind == JNull:
    scriptSettings["summary"] = %* ""
  if scriptSettings.hasKey("description") and scriptSettings["description"].kind == JNull:
    scriptSettings["description"] = %* ""

  client.deployScript(workspace, scriptPath, content, scriptSettings)
  info("✅ Successfully deployed script: " & scriptPath)

proc deployFlowWithRetry*(client: WindmillApiClient, workspace: string, flowPath: string, flowYaml: string, settings: FlowSettings, maxRetries: int, retryDelay: int) =
  ## Deploy a flow with retry logic and linear backoff
  ## Skips deployment if flow already exists (idempotent)

  # Check if flow already exists
  if client.existsFlow(workspace, flowPath):
    info("Flow already exists, skipping deployment: " & flowPath)
    return

  var lastException: ref Exception = nil

  for attempt in 0..maxRetries:
    try:
      deployFlow(client, workspace, flowPath, flowYaml, settings)
      return  # Success, exit retry loop
    except CatchableError as e:
      lastException = e
      if attempt == maxRetries:
        # Final attempt failed, give up
        logging.error("Failed to deploy flow " & flowPath & " after " & $(attempt + 1) & " attempts: " & e.msg)
        raise

      # Fixed delay before retry
      let backoffDelay = retryDelay
      warn("Attempt " & $(attempt + 1) & " failed for flow " & flowPath & ": " & e.msg)
      info("Retrying in " & $backoffDelay & " seconds...")
      sleep(backoffDelay * 1000)

  # Shouldn't reach here, but just in case
  if lastException != nil:
    raise lastException

proc deployScriptWithRetry*(client: WindmillApiClient, workspace: string, scriptPath: string, content: string, settings: ScriptSettings, maxRetries: int, retryDelay: int) =
  ## Deploy a script with retry logic and linear backoff
  ## Skips deployment if script already exists (idempotent)

  # Check if script already exists
  if client.existsScript(workspace, scriptPath):
    info("Script already exists, skipping deployment: " & scriptPath)
    return

  var lastException: ref Exception = nil

  for attempt in 0..maxRetries:
    try:
      deployScript(client, workspace, scriptPath, content, settings)
      return  # Success, exit retry loop
    except CatchableError as e:
      lastException = e
      if attempt == maxRetries:
        # Final attempt failed, give up
        logging.error("Failed to deploy script " & scriptPath & " after " & $(attempt + 1) & " attempts: " & e.msg)
        raise

      # Fixed delay before retry
      let backoffDelay = retryDelay
      warn("Attempt " & $(attempt + 1) & " failed for script " & scriptPath & ": " & e.msg)
      info("Retrying in " & $backoffDelay & " seconds...")
      sleep(backoffDelay * 1000)

  # Shouldn't reach here, but just in case
  if lastException != nil:
    raise lastException

proc createNestedFolders*(client: WindmillApiClient, workspace: string, folderPath: string) =
  ## Create only the top-level folder (Windmill doesn't support nested folders)
  ## Windmill uses virtual folder structure - only top-level folders need to be created
  ## Nested paths in script/flow names work automatically without folder creation
  if folderPath.len == 0:
    return

  # Extract only the first level folder name (Windmill only creates top-level folders)
  let topLevelFolder = folderPath.split("/")[0]

  try:
    client.createFolder(workspace, topLevelFolder)
  except CatchableError as e:
    # If folder creation fails, log but continue - it might already exist
    debug("Folder creation attempt for " & topLevelFolder & " failed: " & e.msg)

proc deployComponentScripts*(client: WindmillApiClient, workspace: string, component: ComponentConfig, baseDir: string, maxRetries: int, retryDelay: int): int =
  ## Deploy all scripts for a component
  ## Returns number of failed deployments
  info("Deploying scripts for component: " & component.name)

  var failures = 0

  # Create nested folders if component target is specified
  if component.target.len > 0:
    createNestedFolders(client, workspace, component.target)

  for scriptSpec in component.scripts:
    var scriptFiles: seq[string]

    if scriptSpec.pattern.len > 0:
      # Pattern-based file discovery - treat pattern as relative path from baseDir
      let patternPath = baseDir / scriptSpec.pattern
      if fileExists(patternPath):
        # Pattern is actually a specific file path (e.g., "reconnaissance/prometheus.py")
        scriptFiles = @[patternPath]
      elif scriptSpec.path.isSome and scriptSpec.path.get().len > 0:
        # Both pattern and path specified - path indicates subdirectory to search (e.g., controller case)
        let searchDir = baseDir / scriptSpec.path.get()
        scriptFiles = findFilesByPattern(searchDir, scriptSpec.pattern)
      else:
        # Pattern is a glob pattern - search in base directory
        scriptFiles = findFilesByPattern(baseDir, scriptSpec.pattern)
    elif scriptSpec.path.isSome and scriptSpec.path.get().len > 0:
      # Direct path override (single file)
      scriptFiles = @[baseDir / scriptSpec.path.get()]
    else:
      # No pattern or path specified
      warn("Script spec has neither pattern nor path, skipping")
      continue

    if scriptFiles.len == 0:
      warn("No files found for script spec: " & scriptSpec.pattern)
      continue

    for scriptFile in scriptFiles:
      # Use component target folder + filename without extension
      let filename = scriptFile.extractFilename()
      let scriptName = filename.changeFileExt("")
      # Build path: f/target_folder/script_name (Windmill requires f/ prefix)
      let windmillPath = "f/" & component.target & "/" & scriptName

      try:
        let content = readScriptContent(scriptFile)
        deployScriptWithRetry(client, workspace, windmillPath, content, scriptSpec.settings, maxRetries, retryDelay)
      except CatchableError as e:
        logging.error("Failed to deploy script " & scriptFile & " after retries: " & e.msg)
        inc(failures)

  return failures

proc deployComponentFlows*(client: WindmillApiClient, workspace: string, component: ComponentConfig, baseDir: string, maxRetries: int, retryDelay: int): int =
  ## Deploy all flows for a component
  ## Returns number of failed deployments
  info("Deploying flows for component: " & component.name)

  var failures = 0

  # Create nested folders if component target is specified
  if component.target.len > 0:
    createNestedFolders(client, workspace, component.target)

  for flowSpec in component.flows.get(@[]):
    var flowFiles: seq[string]

    if flowSpec.pattern.len > 0:
      # Pattern-based flow discovery - treat pattern as relative path from baseDir
      let patternPath = baseDir / flowSpec.pattern
      if fileExists(patternPath):
        # Pattern is actually a specific file path
        flowFiles = @[patternPath]
      elif flowSpec.path.isSome and flowSpec.path.get().len > 0:
        # Both pattern and path specified - path indicates subdirectory to search
        let searchDir = baseDir / flowSpec.path.get()
        flowFiles = findFilesByPattern(searchDir, flowSpec.pattern)
      else:
        # Pattern is a glob pattern - search in base directory
        flowFiles = findFilesByPattern(baseDir, flowSpec.pattern)
    elif flowSpec.path.isSome and flowSpec.path.get().len > 0:
      # Direct path override (single file)
      flowFiles = @[baseDir / flowSpec.path.get()]
    else:
      # No pattern or path specified
      warn("Flow spec has neither pattern nor path, skipping")
      continue

    if flowFiles.len == 0:
      warn("No files found for flow spec: " & flowSpec.pattern)
      continue

    for flowFile in flowFiles:
      # Use component target folder + filename without extension
      let filename = flowFile.extractFilename()
      let flowName = filename.changeFileExt("")
      # Build path: f/target_folder/flow_name (Windmill requires f/ prefix)
      let windmillPath = "f/" & component.target & "/" & flowName

      try:
        let flowYaml = readFlowContent(flowFile)
        deployFlowWithRetry(client, workspace, windmillPath, flowYaml, flowSpec.settings, maxRetries, retryDelay)
      except CatchableError as e:
        logging.error("Failed to deploy flow " & flowFile & " after retries: " & e.msg)
        inc(failures)

  return failures

proc seedWorkspace*(config: SeederConfig): int =
  ## Main seeding function - discover and deploy all components
  ## Returns number of failed deployments
  info("Starting component deployment")

  var totalFailures = 0

  # Create Windmill client
  let windmillConfig = WindmillConfig(
    windmillBaseUrl: config.windmillBaseUrl,
    windmillApiBaseUrl: "",
    windmillWorkspace: config.windmillWorkspace,
    windmillEmail: config.windmillEmail,
    windmillPassword: config.windmillPassword,
    maxRetries: config.maxRetries,
    retryDelay: config.retryDelay
  )

  info("Connecting to Windmill...")
  var client = newWindmillApiClient(windmillConfig)
  info("✅ Successfully authenticated with Windmill")

  # Discover components
  let components = discoverComponents(config)
  info("Found " & $components.len & " components to deploy")

  # Track workspaces to avoid duplicate creation attempts
  var createdWorkspaces = initHashSet[string]()

  # Deploy each component
  for componentInfo in components:
    let component = componentInfo.config
    info("Deploying component: " & component.name)

    # Use the stored directory path from discovery
    let componentDir = componentInfo.directory

    # Use component-specific workspace or default to global workspace
    let targetWorkspace = if component.workspace.len > 0:
                            component.workspace
                          else:
                            config.windmillWorkspace

    info("Deploying to workspace: " & targetWorkspace)

    # Create workspace if not already created
    if not createdWorkspaces.contains(targetWorkspace):
      info("Ensuring workspace exists: " & targetWorkspace)
      client.createWorkspace(targetWorkspace)
      createdWorkspaces.incl(targetWorkspace)

    # Deploy scripts
    if component.scripts.len > 0:
      let scriptFailures = deployComponentScripts(client, targetWorkspace, component, componentDir, config.maxRetries, config.retryDelay)
      totalFailures += scriptFailures

    # Deploy flows
    if component.flows.isSome and component.flows.get().len > 0:
      let flowFailures = deployComponentFlows(client, targetWorkspace, component, componentDir, config.maxRetries, config.retryDelay)
      totalFailures += flowFailures

  if totalFailures > 0:
    logging.error("Component deployment completed with " & $totalFailures & " failures")
  else:
    info("✅ Component deployment completed successfully")

  return totalFailures

proc printHelp* =
  echo """
Godon Seeder - Deploy Godon optimization components to Windmill

Usage:
  godon_seeder [options] [directories...]

Options:
  -h, --help              Show this help message
  -v, --version           Show version information
  --verbose               Enable verbose logging
  --max-retries           Maximum connection retry attempts (default: 30)
  --retry-delay           Delay between retries in seconds (default: 2)

Environment Variables:
  WINDMILL_BASE_URL       Windmill server URL (default: http://localhost:8000)
  WINDMILL_WORKSPACE      Windmill workspace name (default: godon)
  WINDMILL_EMAIL          Windmill admin email (default: admin@windmill.dev)
  WINDMILL_PASSWORD       Windmill admin password (default: changeme)
  SEEDER_MAX_RETRIES      Maximum connection retry attempts (default: 30)
  SEEDER_RETRY_DELAY      Delay between retries in seconds (default: 2)
  GODON_VERSION           Godon version to deploy (default: main)
  GODON_DIR               Godon source directory (default: /godon)
  VERBOSE                 Enable verbose logging (default: false)

Examples:
  # Deploy components from directories
  godon_seeder /godon/controller /godon/breeder

  # Deploy with custom workspace
  WINDMILL_WORKSPACE=my-workspace godon_seeder /godon/controller

  # Deploy with verbose logging and custom retry settings
  godon_seeder --verbose --max-retries=60 /godon/controller

  # Use default GODON_DIR
  godon_seeder
"""

proc printVersion* =
  echo "Godon Seeder v0.1.0"
  echo "Built with Nim "

proc main* =
  var config = loadSeederConfig()

  if config.verbose:
    setLogFilter(lvlDebug)

  var p = initOptParser()

  while true:
    p.next()
    case p.kind
    of cmdEnd: break
    of cmdShortOption, cmdLongOption:
      case p.key
      of "h", "help":
        printHelp()
        quit(0)
      of "v", "version":
        printVersion()
        quit(0)
      of "verbose":
        config.verbose = true
        setLogFilter(lvlDebug)
      of "max-retries":
        if p.val != "":
          config.maxRetries = parseInt(p.val)
        else:
          echo "Error: --max-retries requires a value"
          quit(1)
      of "retry-delay":
        if p.val != "":
          config.retryDelay = parseInt(p.val)
        else:
          echo "Error: --retry-delay requires a value"
          quit(1)
      else:
        echo "Unknown option: " & p.key
        printHelp()
        quit(1)
    of cmdArgument:
      # Treat remaining arguments as source directories
      config.sourceDirs.add(p.key)

  # If no source directories specified, use default godon dir
  if config.sourceDirs.len == 0:
    info("No source directories specified, using GODON_DIR: " & config.godonDir)
    config.sourceDirs.add(config.godonDir)

  info("Starting Godon Seeder")
  info("Source directories: " & config.sourceDirs.join(", "))
  info("Windmill URL: " & config.windmillBaseUrl)
  info("Workspace: " & config.windmillWorkspace)

  debug("Configuration:")
  debug("  Base URL: " & config.windmillBaseUrl)
  debug("  Workspace: " & config.windmillWorkspace)
  debug("  Email: " & config.windmillEmail)
  debug("  Godon Version: " & config.godonVersion)

  # Perform the seeding
  try:
    let failureCount = config.seedWorkspace()
    if failureCount > 0:
      quit(1)  # Exit with error code if any deployments failed
  except CatchableError as e:
    logging.error("Seeding failed: " & e.msg)
    quit(1)

when isMainModule:
  main()
