FROM ghcr.io/windmill-labs/windmill-slim:1.421.2

RUN git clone https://github.com/godon-dev/godon.git

COPY godon_seeder.sh /usr/local/bin/godon_seeder.sh

CMD [ "godon_seeder.sh" ]
