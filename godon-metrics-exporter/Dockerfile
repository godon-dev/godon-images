FROM nimlang/nim:2.0.10-slim

FROM nimlang/nim:2.0.10-regular

ENV EXPORTER_DIR="/user/local/godon_exporter/"

# install the nim prometheus-client implementation
# install the nim postgres client lib
RUN nimble install metrics && \
    nimble install db_connector

RUN apt update && \
    apt install -y libpq5 && \
    apt clean

# Place nim code
COPY ./*.nim "${EXPORTER_DIR}"

# Compile Exporter
RUN cd "${EXPORTER_DIR}" && \
    nim compile --threads:on -d:release -d:metrics exporter.nim

# Install Exporter Binary
RUN install "${EXPORTER_DIR}/exporter" "/usr/local/bin/godon-metrics-exporter"

EXPOSE 8089

ENTRYPOINT godon-metrics-exporter --port="${PORT}"
