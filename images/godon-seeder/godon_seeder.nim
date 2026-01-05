import std/[parseopt, strutils, os, logging, json, times, tables, sequtils, sets]
import yaml
import windmill_client.config
import windmill_client.windmill_client

addHandler(newConsoleLogger())
setLogFilter(lvlInfo)

type
  # Component configuration types
  ScriptSettings* = object
    summary*: string
    description*: string
    timeout*: Option[int]

  ScriptSpec* = object
    pattern*: string
    path*: string
    settings*: ScriptSettings

  ComponentConfig* = object
    name*: string
    target*: string
    workspace*: Option[string]
    scripts*: seq[ScriptSpec]

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
  result = ScriptSettings(
    summary: settingsJson.getOrDefault("summary").getStr(""),
    description: settingsJson.getOrDefault("description").getStr(""),
    timeout: if settingsJson.hasKey("timeout"): some(settingsJson["timeout"].getInt()) else: none(int)
  )

proc parseScriptSpec(specJson: JsonNode): ScriptSpec =
  ## Parse script specification from JSON node
  let settingsJson = specJson.getOrDefault("settings")
  result = ScriptSpec(
    pattern: specJson.getOrDefault("pattern").getStr(""),
    path: specJson.getOrDefault("path").getStr(""),
    settings: if settingsJson != nil and settingsJson.kind == JObject: parseScriptSettings(settingsJson) else: ScriptSettings()
  )

proc parseComponentConfig(yamlPath: string): ComponentConfig =
  ## Parse component.yaml file into ComponentConfig object
  info("Parsing component config: " & yamlPath)

  let yamlContent = readFile(yamlPath)

  # Use yaml.load to directly parse into ComponentConfig
  try:
    load(yamlContent, result)
    info("Parsed component '" & result.name & "' with " & $result.scripts.len & " scripts")
  except CatchableError as e:
    logging.error("Failed to parse YAML: " & e.msg)
    # Return empty config as fallback
    result = ComponentConfig(
      name: yamlPath.splitPath().head,
      target: "",
      workspace: none(string),
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

    # Look for component.yaml files
    for kind, path in walkDir(sourceDir, relative=true):
      if kind == pcFile and path.endsWith("component.yaml"):
        let fullPath = sourceDir / path
        info("Found component config: " & fullPath)
        try:
          let component = parseComponentConfig(fullPath)
          # Create ComponentInfo with directory
          let componentInfo = ComponentInfo(
            config: component,
            directory: sourceDir
          )
          result.add(componentInfo)
        except CatchableError as e:
          logging.error("Failed to parse component config " & fullPath & ": " & e.msg)

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

proc readScriptContent(scriptPath: string): string =
  ## Read script content and determine language from extension
  if not fileExists(scriptPath):
    raise newException(IOError, "Script file not found: " & scriptPath)

  result = readFile(scriptPath)
  debug("Read script content from: " & scriptPath & " (" & $result.len & " bytes)")

proc deployScript*(client: WindmillApiClient, workspace: string, scriptPath: string, content: string, settings: ScriptSettings) =
  ## Deploy a single script to Windmill
  info("Deploying script: " & scriptPath)

  # Build script settings JSON
  var scriptSettings = newJObject()
  if settings.summary.len > 0:
    scriptSettings["summary"] = %* settings.summary
  if settings.description.len > 0:
    scriptSettings["description"] = %* settings.description
  if settings.timeout.isSome:
    scriptSettings["timeout"] = %* settings.timeout.get()

  client.deployScript(workspace, scriptPath, content, scriptSettings)
  info("✅ Successfully deployed script: " & scriptPath)

proc deployComponentScripts*(client: WindmillApiClient, workspace: string, component: ComponentConfig, baseDir: string) =
  ## Deploy all scripts for a component
  info("Deploying scripts for component: " & component.name)

  for scriptSpec in component.scripts:
    var scriptFiles: seq[string]

    if scriptSpec.pattern.len > 0 and scriptSpec.path.len > 0:
      # Pattern-based discovery within specified subdirectory
      let searchDir = baseDir / scriptSpec.path
      scriptFiles = findFilesByPattern(searchDir, scriptSpec.pattern)
    elif scriptSpec.path.len > 0:
      # Direct path override (single file)
      scriptFiles = @[baseDir / scriptSpec.path]
    elif scriptSpec.pattern.len > 0:
      # Pattern-based discovery in base directory
      scriptFiles = findFilesByPattern(baseDir, scriptSpec.pattern)
    else:
      # No pattern or path specified
      warn("Script spec has neither pattern nor path, skipping")
      continue

    if scriptFiles.len == 0:
      warn("No files found for script spec: " & scriptSpec.pattern)
      continue

    for scriptFile in scriptFiles:
      let relativePath = scriptFile.relativePath(baseDir)
      let windmillPath = if component.target.len > 0:
                          component.target / relativePath
                        else:
                          relativePath

      try:
        let content = readScriptContent(scriptFile)
        deployScript(client, workspace, windmillPath, content, scriptSpec.settings)
      except CatchableError as e:
        logging.error("Failed to deploy script " & scriptFile & ": " & e.msg)

proc seedWorkspace*(config: SeederConfig) =
  ## Main seeding function - discover and deploy all components
  info("Starting component deployment")

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
    let targetWorkspace = if component.workspace.isSome:
                            component.workspace.get()
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
      deployComponentScripts(client, targetWorkspace, component, componentDir)

  info("✅ Component deployment completed successfully")

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
    config.seedWorkspace()
  except CatchableError as e:
    logging.error("Seeding failed: " & e.msg)
    quit(1)

when isMainModule:
  main()
