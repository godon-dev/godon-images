
import metrics, times

proc get_godon_metric(): int =
  result = 0

when defined(metrics):
  type godonCollector = ref object of Collector
  let godonCollector  = godonCollector.newCollector(name = "godon_metrics", help = "Offers metrics from internas of the godon logic.")

  method collect(collector: PowerCollector, output: MetricHandler): Metrics =
    let timestamp = collector.now()
    output(
      name = "godon_metrics",
      value = ,
      timestamp = timestamp,
    )
