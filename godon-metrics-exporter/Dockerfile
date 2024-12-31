FROM nimlang/nim:2.0.10-slim

ENV EXPORTER_DIR="/user/local/godon_exporter"

# install the nim prometheus-client implementation
RUN nimble install metrics

# Place nim code
COPY ./*.nim "${EXPORTER_DIR}"

# Compile Exporter
RUN cd "${EXPORTER_DIR}" \
    nim -c -d:release exporter.nim

# Install Exporter Binary
RUN install "${EXPORTER_DIR}/exporter" "/usr/local/bin/godon-metrics-exporter"

EXPOSE 8089

ENTRYPOINT godon-metrics-exporter --port "${PORT}" --config "{CONFIG_FILE}"
