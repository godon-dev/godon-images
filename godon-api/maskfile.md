<!--
Copyright (c) 2019 Matthias Tafelmeier.

This file is part of godon

godon is free software: you can redistribute it and/or modify
it under the terms of the GNU Affero General Public License as
published by the Free Software Foundation, either version 3 of the
License, or (at your option) any later version.

godon is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
GNU Affero General Public License for more details.

You should have received a copy of the GNU Affero General Public License
along with this godon. If not, see <http://www.gnu.org/licenses/>.
-->
# Godon API

## api 

> micro infrastructure stack related logic

### api validate

> validate the openapi spec

~~~bash
set -eEux

docker run --rm -v "${PWD}:/local" openapitools/openapi-generator-cli:v7.1.0 validate -i /local/openapi.yml
~~~

### api generate

> generate server stub from the openapi spec

~~~bash
set -eEux

docker run --rm -v "${MASKFILE_DIR}:/local" openapitools/openapi-generator-cli:v7.1.0 generate \
            -i /local/openapi.yml \
            --template-dir /local/templates/ \
            --generator-name python-flask \
            -o /local/flask

cp "${MASKFILE_DIR}"/controller.py ./flask/openapi_server/controllers/controller.py
cp "${MASKFILE_DIR}"/archive_db.py ./flask/openapi_server/controllers/archive_db.py
cp "${MASKFILE_DIR}"/meta_data_db.py ./flask/openapi_server/controllers/meta_data_db.py
touch ./flask/openapi_server/controllers/__init__.py

# constrain connexion to below 3.0.0
echo 'connexion[swagger-ui] <= 2.14.2; python_version>"3.5"' >> ./flask/requirements.txt

 # pin base image version
sed -i -E 's:3-alpine:3.10.13-alpine3.19:g' ./flask/Dockerfile
~~~
