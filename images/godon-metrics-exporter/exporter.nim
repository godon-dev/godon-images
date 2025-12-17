
import metrics
import metrics/chronos_httpserver
import net
import os

import std/tables
import std/parseopt
import std/strutils

import db_connector/db_postgres

const ARCHIVE_DB_USER = getEnv("ARCHIVE_DB_USER")
const ARCHIVE_DB_PW = getEnv("ARCHIVE_DB_PW")
const ARCHIVE_DB_HOST = getEnv("ARCHIVE_DB_HOST")
const ARCHIVE_DB_DATABASE_NAME = getEnv("ARCHIVE_DB_DATABASE_NAME")

proc parse_args(): Table[string, string] =
  var args = initTable[string, string]()
  
  # Set defaults
  args["host"] = "127.0.0.1"
  args["port"] = "8089"

  for kind, key, val in getopt():
    case kind
    of cmdArgument:
      discard
    of cmdLongOption, cmdShortOption:
      case key:
      of "host": # --host:<value> binding address
        args["host"] = val
      of "port": # --port:<value> binding port
        args["port"] = val
      of "help":
        echo "Usage: godon-metrics-exporter [options]"
        echo "Options:"
        echo "  --host:HOST     Bind address (default: 127.0.0.1)"
        echo "  --port:PORT     Bind port (default: 8089)"
        echo "  --help          Show this help message"
        echo ""
        echo "Environment variables:"
        echo "  ARCHIVE_DB_HOST         PostgreSQL host"
        echo "  ARCHIVE_DB_USER         PostgreSQL user"
        echo "  ARCHIVE_DB_PW           PostgreSQL password"
        echo "  ARCHIVE_DB_DATABASE_NAME PostgreSQL database name"
        quit(0)
    of cmdEnd:
      discard

  result = args


when defined(metrics):
  type GodonCollector = ref object of Collector
  let godonCollector  = GodonCollector.newCollector(name = "godon_metrics", help = "Offers metrics from internas of the godon logic.")

  method collect(collector: GodonCollector, output: MetricHandler) =
    let timestamp = collector.now()

    try:
      # connect to godon archive DB
      let db = open(ARCHIVE_DB_HOST,
                    ARCHIVE_DB_USER,
                    ARCHIVE_DB_PW,
                    ARCHIVE_DB_DATABASE_NAME)
      defer: db.close()

      # query all breeder tables row count from archive db
      let sql_qery = sql"SELECT relname, n_live_tup FROM pg_stat_user_tables;"
      var breeder_tables_row_count_list = db.getAllRows(sql_qery)

      for row in breeder_tables_row_count_list:
        let breeder_table_name = row[0]
        let settings_count = row[1]

        output(
          name = "godon_breeder_settings_explored",
          labels = @["breeder_id"],
          labelValues = @[breeder_table_name],
          value = parseFloat(settings_count),
          timestamp = timestamp
        )
    except CatchableError as e:
      echo e.msg

var args = parse_args()

echo "Starting godon-metrics-exporter..."
echo "Binding to $1:$2" % [ args["host"], args["port"] ]

chronos_httpserver.startMetricsHttpServer(args["host"], Port(parseInt(args["port"])))

## Todo: improve the loop in the main thread with something
## more threading native
while true:
  sleep(10000)
