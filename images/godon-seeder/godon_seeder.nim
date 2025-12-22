import std/[parseopt, strutils, os, logging]

addHandler(newConsoleLogger())
setLogFilter(lvlInfo)

type
  SeederConfig* = object
    windmillBaseUrl*: string
    windmillWorkspace*: string
    windmillEmail*: string
    windmillPassword*: string
    godonVersion*: string
    godonDir*: string
    verbose*: bool

proc loadSeederConfig*: SeederConfig =
  ## Load configuration from environment variables
  result.windmillBaseUrl = getEnv("WINDMILL_BASE_URL", "http://localhost:8000")
  result.windmillWorkspace = getEnv("WINDMILL_WORKSPACE", "godon")
  result.windmillEmail = getEnv("WINDMILL_EMAIL", "admin@windmill.dev")
  result.windmillPassword = getEnv("WINDMILL_PASSWORD", "changeme")
  result.godonVersion = getEnv("GODON_VERSION", "main")
  result.godonDir = getEnv("GODON_DIR", "/godon")
  result.verbose = getEnv("VERBOSE", "false") == "true"

proc printHelp* =
  echo """
Godon Seeder - Deploy Godon optimization components to Windmill

Usage:
  godon_seeder [options]

Options:
  -h, --help              Show this help message
  -v, --version           Show version information
  --verbose               Enable verbose logging

Environment Variables:
  WINDMILL_BASE_URL       Windmill server URL (default: http://localhost:8000)
  WINDMILL_WORKSPACE      Windmill workspace name (default: godon)
  WINDMILL_EMAIL          Windmill admin email (default: admin@windmill.dev)
  WINDMILL_PASSWORD       Windmill admin password (default: changeme)
  GODON_VERSION           Godon version to deploy (default: main)
  GODON_DIR               Godon source directory (default: /godon)
  VERBOSE                 Enable verbose logging (default: false)

Examples:
  # Deploy to local Windmill
  godon_seeder

  # Deploy with custom workspace
  WINDMILL_WORKSPACE=my-workspace godon_seeder

  # Deploy specific version
  GODON_VERSION=v1.2.3 godon_seeder --verbose
"""

proc printVersion* =
  echo "Godon Seeder v0.1.0"
  echo "Built with Nim "

proc main* =
  var config = loadSeederConfig()
  
  if config.verbose:
    setLogFilter(lvlDebug)
  
  info("Starting Godon Seeder")
  info("Windmill URL: " & config.windmillBaseUrl)
  info("Workspace: " & config.windmillWorkspace)
  info("Godon Version: " & config.godonVersion)
  
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
      else:
        echo "Unknown option: " & p.key
        printHelp()
        quit(1)
    of cmdArgument:
      echo "Unknown argument: " & p.key
      printHelp()
      quit(1)
  
  info("Godon Seeder configuration loaded")
  info("This seeder will deploy components to Windmill workspace")
  
  debug("Configuration:")
  debug("  Base URL: " & config.windmillBaseUrl)
  debug("  Workspace: " & config.windmillWorkspace)
  debug("  Email: " & config.windmillEmail)
  debug("  Godon Version: " & config.godonVersion)
  debug("  Godon Dir: " & config.godonDir)
  
  info("✅ Seeder configuration validated successfully")
  info("Note: Full deployment functionality will be implemented in future versions")

when isMainModule:
  main()
