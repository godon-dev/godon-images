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


WINDMILL_APP_SERVICE_HOST = os.environ['WINDMILL_APP_SERVICE_HOST']
WINDMILL_APP_SERVICE_PORT = os.environ['WINDMILL_APP_SERVICE_PORT']

WINDMILL_WORKSPACE="godon"
WINDMILL_FOLDER="controller"

WINDMILL_BASE_URL=f"http://{WINDMILL_APP_SERVICE_HOST}:{WINDMILL_APP_SERVICE_PORT}"
WINDMILL_API_BASE_URL=f"{WINDMILL_BASE_URL}/api/w/{WINDMILL_WORKSPACE}/jobs/run/p/f/{WINDMILL_FOLDER}"

def windmill_perform_login():

    url = f"{WINDMILL_BASE_URL}/api/auth/login"

    payload = { "email": "admin@windmill.dev", "password": "changeme" }
    headers = {
        "Content-Type": "application/json",
        }

    response = requests.post(url, json=payload, headers=headers)
    response.raise_for_status()

    token = response.text

    return token


def breeders_id_delete(uuid):  # noqa: E501
    """breeders_delete

    Purge a breeder # noqa: E501

    """
    url = f"{WINDMILL_API_BASE_URL}/breeder_delete"
    token = windmill_perform_login()

    payload = { "breeder_id": uuid }
    headers = {
        "Content-Type": "application/json",
        "Authorization": f"Bearer {token}"
        }

    response = requests.post(url, json=payload, headers=headers)
    response.raise_for_status()

    return Response(json.dumps(dict(message=f"Purged Breeder named {breeder_id}")),
                    status=200,
                    mimetype='application/json')

def breeders_get():  # noqa: E501
    """breeders_get

    Provides info on configured breeders # noqa: E501

    """
    configured_breeders = list()

    url = f"{WINDMILL_API_BASE_URL}/breeders_get"
    token = windmill_perform_login()

    payload = { }
    headers = {
        "Content-Type": "application/json",
        "Authorization": f"Bearer {token}"
        }

    response = requests.post(url, json=payload, headers=headers)
    response.raise_for_status()

    response_data = response.json()

    configured_breeders = response_data.get("breeders")

    return Response(response=json.dumps(configured_breeders),
                    status=200,
                    mimetype='application/json')


def breeder_id_get(uuid):  # noqa: E501
    """breeders_name_get

    Obtain information about breeder from its name # noqa: E501

    """

    url = f"{WINDMILL_API_BASE_URL}/breeder_get"
    token = windmill_perform_login()

    payload = { "breeder_id": uuid }
    headers = {
        "Content-Type": "application/json",
        "Authorization": f"Bearer {token}"
        }

    response = requests.post(url, json=payload, headers=headers)
    response.raise_for_status()

    response_data = response.json()

    configured_breeders = response_data.get("breeders")

    return Response(response=json.dumps(configured_breeders),
                    status=200,
                    mimetype='application/json')


def breeders_post(content):  # noqa: E501
    """breeders_post

    Create a breeder # noqa: E501

    """

    breeder_config_full = content

    url = f"{WINDMILL_API_BASE_URL}/breeder_create"
    token = windmill_perform_login()

    payload = { "breeder_config": breeder_config_full }
    headers = {
        "Content-Type": "application/json",
        "Authorization": f"Bearer {token}"
        }

    response = requests.post(url, json=payload, headers=headers)
    response.raise_for_status()

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
