#
# Copyright (c) 2019 Matthias Tafelmeier.
#
# This file is part of godon
#
# godon is free software: you can redistribute it and/or modify
# it under the terms of the GNU Affero General Public License as
# published by the Free Software Foundation, either version 3 of the
# License, or (at your option) any later version.
#
# godon is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU Affero General Public License for more details.
#
# You should have received a copy of the GNU Affero General Public License
# along with this godon. If not, see <http://www.gnu.org/licenses/>.



import requests
import os
import time
import datetime
import hashlib
from pprint import pprint
from dateutil.parser import parse as dateutil_parser

from flask import abort
from flask import Response

import logging

import json
import uuid


def windmill_perform_login():

    url = "https://app.windmill.dev/api/auth/login" #

    payload = { "email": "admin@windmill.dev", "password": "changeme" }
    headers = {
        "Content-Type": "application/json",
        }

    response = requests.post(url, json=payload, headers=headers)

    response_data = response.json()

    token = response_data.get("token")

    return token


def breeders_id_delete(breeder_id):  # noqa: E501
    """breeders_delete

    Purge a breeder # noqa: E501

    """

    url = "https://app.windmill.dev/api/w/godon/jobs/run/f/godon/breeder_delete.py"
    token = windmill_perform_login()

    payload = { "breeder_id": breeder_id }
    headers = {
        "Content-Type": "application/json",
        "Authorization": f"Bearer {token}"
        }

    response = requests.post(url, json=payload, headers=headers)

    return Response(json.dumps(dict(message=f"Purged Breeder named {breeder_id}")),
                    status=200,
                    mimetype='application/json')

def breeders_get():  # noqa: E501
    """breeders_get

    Provides info on configured breeders # noqa: E501

    """
    configured_breeders = list()

    url = "https://app.windmill.dev/api/w/godon/jobs/run/f/godon/breeders_list.py"
    token = windmill_perform_login()

    payload = { }
    headers = {
        "Content-Type": "application/json",
        "Authorization": f"Bearer {token}"
        }

    response = requests.post(url, json=payload, headers=headers)

    response_data = response.json()

    configured_breeders = response_data.get("breeders")

    return Response(response=json.dumps(configured_breeders),
                    status=200,
                    mimetype='application/json')


def breeders_id_get(breeder_uuid):  # noqa: E501
    """breeders_name_get

    Obtain information about breeder from its name # noqa: E501

    """

    url = "https://app.windmill.dev/api/w/godon/jobs/run/f/godon/breeder_get.py"
    token = windmill_perform_login()

    payload = { "breeder_id": breeder_uuid }
    headers = {
        "Content-Type": "application/json",
        "Authorization": f"Bearer {token}"
        }

    response = requests.post(url, json=payload, headers=headers)

    response_data = response.json()

    return_data = dict(breeder_creation_ts=response_data.get("breeder_creation_timestamp"),
                       breeder_definition=response_data.get("breeder_definition"))

    return Response(response=json.dumps(return_data),
                    status=200,
                    mimetype='application/json')

def breeders_post(content):  # noqa: E501
    """breeders_post

    Create a breeder # noqa: E501

    """


    breeder_config_full = content
    breeder_config = dict(content.get('breeder'))
    breeder_name = breeder_config.get('name')
    uuid = uuid.uuid4()
    config.update(dict(uuid=uuid))

    url = "https://app.windmill.dev/api/w/godon/jobs/run/f/godon/breeder_create.py"
    token = windmill_perform_login()

    payload = { "config": content }
    headers = {
        "Content-Type": "application/json",
        "Authorization": f"Bearer {token}"
        }

    response = requests.post(url, json=payload, headers=headers)

    response_data = response.json()

    breeder_id = response_data.get("breeder_id")

    return Response(json.dumps(dict(message=f"Created Breeder named {breeder_id}")),
                               status=200,
                               mimetype='application/json')


def breeders_put(content):  # noqa: E501
    """breeders_put

    Update a breeder configuration # noqa: E501

    """
    abort(501, description="Not Implemented")
