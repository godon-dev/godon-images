FROM nimlang/nim:2.0.10-slim

EXPOSE 8089

ENTRYPOINT godon-metrics-exporter --port "${PORT}" --config "{CONFIG_FILE}"
