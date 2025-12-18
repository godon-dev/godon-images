# Package
version       = "0.0.1"
author        = "Godon Project"
description   = "Godon Metrics Exporter - Prometheus exporter for Godon archive database"
license       = "AGPL-3.0"
srcDir        = "."

# Dependencies
requires "nim >= 2.0.10"
requires "metrics"
requires "db_connector"
requires "chronos"

bin           = @["exporter"]