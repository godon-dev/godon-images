
import metrics
import metrics/chronos_httpserver
import times
import net

import std/tables
import std/parseopt
import std/strutils


proc parse_args(): Table[string, string] =
  var args = initTable[string, string]()

  for kind, key, val in getopt():
    case kind
    of cmdArgument:
      discard
    of cmdLongOption, cmdShortOption:
      case key:
      of "port": # --varName:<value> in the console when executing
        args["port"] = val # do input sanitization in production systems
    of cmdEnd:
      discard

  result = args


proc get_godon_metric(): int =
  result = 0

when defined(metrics):
  type godonCollector = ref object of Collector
  let godonCollector  = godonCollector.newCollector(name = "godon_metrics", help = "Offers metrics from internas of the godon logic.")

  method collect(collector: godonCollector, output: MetricHandler): Metrics =
    let timestamp = collector.now()
    output(
      name = "godon_metrics",
      value = get_godon_metric(),
      timestamp = timestamp,
    )

var args = parse_args()

startMetricsHttpServer("127.0.0.1", Port(parseInt(args["port"])))
