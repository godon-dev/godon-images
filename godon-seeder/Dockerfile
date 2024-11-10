FROM ghcr.io/windmill-labs/windmill-slim:1.421.2

ENV GODON_REPO_URL="https://github.com/godon-dev/godon.git"
ENV GODON_DIR="/usr/local/share/godon/"

RUN git clone "${GODON_REPO_URL}" "${GODON_DIR}"

COPY godon_seeder.sh /usr/local/bin/godon_seeder.sh

CMD [ "godon_seeder.sh" ]
