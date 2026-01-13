
##

import std/[os, strutils, logging, parseopt, tables]
import metrics
import metrics/chronos_httpserver

const
  version {.strdefine.} = "0.0.1"  # Can be overridden with -d:version="X.Y.Z"
  appName = "Godon Metrics Exporter (Nim)"

type
  ExporterConfig = ref object
    host: string
    port: int
    dbHost: string
    dbUser: string
    dbPassword: string
    dbName: string

proc setupLogging*(level: Level = lvlInfo) =
  ## Setup console logging with proper formatting
  let fmtStr = "$datetime [$levelname] "
  var logger = newConsoleLogger(levelThreshold = level, fmtStr = fmtStr)
  addHandler(logger)

proc printVersion*() =
  ## Print version information
  echo appName, " v", version
  echo "Built with Nim - high performance systems programming language"

proc printUsage*() =
  ## Print usage information
  echo """
Usage: godon-metrics-exporter [options]

Options:
  -h, --help              Show this help message
  -v, --version           Show version information
  -H, --host:HOST         Bind address (default: 127.0.0.1)
  -p, --port:PORT         HTTP server port (default: 8089)
  --log-level:LEVEL       Logging level (DEBUG, INFO, WARN, ERROR)

Environment Variables:
  ARCHIVE_DB_HOST         PostgreSQL host (required)
  ARCHIVE_DB_USER         PostgreSQL user (required)
  ARCHIVE_DB_PW           PostgreSQL password (required)
  ARCHIVE_DB_DATABASE_NAME PostgreSQL database name (required)

Examples:
  godon-metrics-exporter                                    # Run with defaults
  godon-metrics-exporter -H 0.0.0.0 -p 9090                 # Custom host and port
  godon-metrics-exporter --log-level:DEBUG                  # Debug logging

Database Configuration:
  The exporter connects to a PostgreSQL database to collect metrics about
  breeder tables. All database connection parameters must be provided via
  environment variables.
"""

proc loadConfig*(host: string, port: int): ExporterConfig =
  ## Load configuration from arguments and environment
  let dbHost = getEnv("ARCHIVE_DB_HOST", "")
  let dbUser = getEnv("ARCHIVE_DB_USER", "")
  let dbPassword = getEnv("ARCHIVE_DB_PW", "")
  let dbName = getEnv("ARCHIVE_DB_DATABASE_NAME", "")
  
  # Validate required environment variables
  if dbHost.len == 0:
    error "ARCHIVE_DB_HOST environment variable is required"
    quit(QuitFailure)
  if dbUser.len == 0:
    error "ARCHIVE_DB_USER environment variable is required"
    quit(QuitFailure)
  if dbPassword.len == 0:
    error "ARCHIVE_DB_PW environment variable is required"
    quit(QuitFailure)
  if dbName.len == 0:
    error "ARCHIVE_DB_DATABASE_NAME environment variable is required"
    quit(QuitFailure)
  
  result = ExporterConfig(
    host: host,
    port: port,
    dbHost: dbHost,
    dbUser: dbUser,
    dbPassword: dbPassword,
    dbName: dbName
  )

proc parseArgs(): Table[string, string] =
  var args = initTable[string, string]()

  for kind, key, val in getopt():
    case kind
    of cmdArgument:
      discard
    of cmdLongOption, cmdShortOption:
      case key:
      of "host", "H":
        args["host"] = val
      of "port", "p":
        args["port"] = val
      of "log-level":
        args["log-level"] = val
      of "help", "h":
        printVersion()
        echo ""
        printUsage()
        quit(QuitSuccess)
      of "version", "v":
        printVersion()
        quit(QuitSuccess)
    of cmdEnd:
      break

  return args


when defined(metrics):
  type GodonCollector = ref object of Collector
    config: ExporterConfig

  var godonCollector: GodonCollector

  proc simpleCollector(): Collector =
    ## Simple collector that doesn't use inheritance for testing
    result = Collector.newCollector(name = "godon_metrics_simple", help = "Simple Godon metrics exporter")

  # Removed newGodonCollector - done directly in initGodonCollector (following prometheus_ss_exporter pattern)

  method collect(collector: GodonCollector, output: MetricHandler) =
    let timestamp = collector.now()
    
    # Simple success metric with minimal processing for collector thread stability
    output(
      name = "godon_metrics_exporter_status",
      labels = @["status"],
      labelValues = @["success"],
      value = 1.0,
      timestamp = timestamp
    )
    
    # TODO: Replace with actual database metrics collection
    # The following code is commented out to avoid database connection requirements
    # during initial testing. Uncomment when database connectivity is needed.
    #
    # proc collectActualMetrics(output: MetricHandler, timestamp: Time) =
    #   try:
    #     import db_connector/db_postgres
    #     debug "Connecting to database: ", collector.config.dbHost, "/", collector.config.dbName
    #     # connect to godon archive DB
    #     let db = open(collector.config.dbHost,
    #                   collector.config.dbUser,
    #                   collector.config.dbPassword,
    #                   collector.config.dbName)
    #     defer: db.close()
    # 
    #     debug "Connected to database successfully"
    # 
    #     # query all breeder tables row count from archive db
    #     let sqlQuery = sql"SELECT relname, n_live_tup FROM pg_stat_user_tables WHERE relname LIKE 'breeder_%';"
    #     var breederTablesRowCountList = db.getAllRows(sqlQuery)
    # 
    #     debug "Found ", $breederTablesRowCountList.len, " breeder tables"
    # 
    #     for row in breederTablesRowCountList:
    #       let breederTableName = row[0]
    #       let settingsCount = row[1]
    # 
    #       # Extract breeder ID from table name (assuming format: breeder_XXXXXXXXXXXX)
    #       let breederId = if breederTableName.len > 8: breederTableName[8..^1] else: breederTableName
    # 
    #       debug "Exporting metric for breeder: ", breederId, " count: ", settingsCount
    # 
    #       output(
    #         name = "godon_breeder_settings_explored",
    #         labels = @["breeder_id"],
    #         labelValues = @[breederId],
    #         value = parseFloat(settingsCount),
    #         timestamp = timestamp
    #       )
    # 
    #     info "Successfully exported ", $breederTablesRowCountList.len, " breeder metrics"
    # 
    #   except CatchableError as e:
    #     error "Failed to collect metrics: ", e.msg
    #     error "Database connection error - check connection parameters"
    #     debug "Exception details: ", e.name, ": ", e.getStackTrace()
  
  proc initGodonCollector(config: ExporterConfig) =
    ## Initialize the Godon metrics collector
    godonCollector = GodonCollector.newCollector(name = "godon_metrics", help = "Offers metrics from internals of the godon logic.")
    godonCollector.config = config

proc main*(host = "127.0.0.1", port = 8089, logLevel = "INFO") =
  ## Main entry point for the Godon metrics exporter

  # Setup logging
  let level = case logLevel.toUpperAscii():
    of "DEBUG": lvlDebug
    of "INFO": lvlInfo
    of "WARN": lvlWarn
    of "ERROR": lvlError
    else: lvlInfo

  setupLogging(level)

  info "Starting ", appName, " v", version
  info "Nim compiler version: ", NimVersion
  info "Process ID: ", getCurrentProcessId()

  # Load and validate configuration
  let config = loadConfig(host, port)
  info "Configuration loaded successfully"
  info "Database host: ", config.dbHost
  info "Database name: ", config.dbName
  info "Bind address: ", config.host, ":", config.port

  # Initialize metrics collector
  when defined(metrics):
    initGodonCollector(config)

  # Start Prometheus metrics HTTP server
  info "Starting Prometheus metrics HTTP server..."
  try:
    # Follow prometheus_ss_exporter pattern exactly
    chronos_httpserver.startMetricsHttpServer(config.host, Port(config.port))
    info "Prometheus metrics HTTP server started successfully"
    info "Metrics endpoint: http://", config.host, ":", config.port, "/metrics"

    # Keep the main thread alive (following prometheus_ss_exporter pattern)
    while true:
      sleep(1000)

  except CatchableError as e:
    error "Failed to start metrics HTTP server: ", e.msg
    error "Exception details: ", e.name, ": ", e.msg
    error "Stack trace: ", e.getStackTrace()
    quit(QuitFailure)

when isMainModule:
  # Parse command line arguments
  let args = parseArgs()

  # Set defaults and override with parsed arguments
  var host = "127.0.0.1"
  var port = 8089
  var logLevel = "INFO"

  # Apply parsed arguments with validation
  if args.contains("host"):
    let hostStr = args["host"]
    if hostStr.len == 0:
      echo "Error: --host requires a value"
      quit(QuitFailure)
    host = hostStr

  if args.contains("port"):
    let portStr = args["port"]
    if portStr.len == 0:
      echo "Error: --port requires a numeric value"
      quit(QuitFailure)
    try:
      port = parseInt(portStr)
      if port <= 0 or port > 65535:
        echo "Error: --port must be between 1 and 65535, got: ", portStr
        quit(QuitFailure)
    except ValueError:
      echo "Error: --port requires a valid numeric value, got: ", portStr
      quit(QuitFailure)

  if args.contains("log-level"):
    let logStr = args["log-level"]
    if logStr.len == 0:
      echo "Error: --log-level requires a value (DEBUG, INFO, WARN, ERROR)"
      quit(QuitFailure)
    logLevel = logStr

  main(host, port, logLevel)
