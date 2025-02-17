FROM nimlang/nim:2.0.10-slim

FROM nimlang/nim:2.0.10-regular

ENV EXPORTER_DIR="/user/local/godon_exporter/"

# install the nim prometheus-client implementation
# install the nim postgres client lib
RUN nimble install metrics && \
    nimble install db_connector

# Place nim code
COPY ./*.nim "${EXPORTER_DIR}"

# Compile Exporter
RUN cd "${EXPORTER_DIR}" && \
    nim compile -d:release exporter.nim

# Install Exporter Binary
RUN install "${EXPORTER_DIR}/exporter" "/usr/local/bin/godon-metrics-exporter"

EXPOSE 8089

ENTRYPOINT godon-metrics-exporter --port="${PORT}" --config="{CONFIG_FILE}"

