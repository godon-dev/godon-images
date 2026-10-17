def main(verb=None, request_data=None):
    """Stub for the controller's fast lane entry - the verb rides the
    arguments; every handler below is the per-verb stub body, verbatim."""
    handler = HANDLERS.get(verb)
    if handler is None:
        return {"result": "FAILURE", "error": f"unknown verb: {verb}"}
    return handler(request_data)


def _systemtenders_get(request_data):
    """Stub for systemtenders_get - returns list of systemtenders"""
    return {
        "result": "SUCCESS",
        "data": [
            {
                "id": "test-systemtender-1",
                "name": "test-systemtender",
                "status": "active",
                "createdAt": "2024-01-01T00:00:00Z"
            }
        ]
    }


def _systemtender_create(request_data):
    """Stub for systemtender_create - creates a new systemtender"""
    if not request_data:
        return {"result": "FAILURE", "error": "Missing request data"}

    name = request_data.get("name")
    if not name:
        return {"result": "FAILURE", "error": "Missing required field: name"}

    # Validate name format (no spaces)
    if " " in name:
        return {"result": "FAILURE", "error": "Invalid name format: spaces not allowed"}

    config = request_data.get("config")
    if not config:
        return {"result": "FAILURE", "error": "Missing required field: config"}

    return {
        "result": "SUCCESS",
        "data": {
            "id": "test-systemtender-2",
            "name": name,
            "status": "active",
            "createdAt": "2024-01-01T00:00:00Z"
        }
    }


def _systemtender_get(request_data):
    """Stub for systemtender_get - gets a specific systemtender"""
    systemtender_id = request_data.get("systemtender_id") if request_data else None

    # Special UUIDs for testing non-existent systemtenders
    if systemtender_id in ["00000000-0000-4000-8000-000000000000", "99999999-9999-4999-9999-999999999999"]:
        return {
            "result": "FAILURE",
            "error": f"Systemtender with ID '{systemtender_id}' not found"
        }

    # For any other UUID, return wrapped systemtender object
    return {
        "result": "SUCCESS",
        "data": {
            "id": systemtender_id,
            "name": "test-systemtender",
            "status": "active",
            "createdAt": "2024-01-01T00:00:00Z",
            "config": {
                "type": "linux_performance"
            }
        }
    }


def _systemtender_stop(request_data):
    """Stub for systemtender_stop - stops a systemtender's workers"""
    systemtender_id = request_data.get("systemtender_id") if request_data else None

    if not systemtender_id:
        return {"result": "FAILURE", "error": "Missing systemtender_id parameter"}

    # Simulate stop (always succeeds for stub)
    return {
        "result": "SUCCESS",
        "data": {
            "systemtender_id": systemtender_id,
            "shutdown_type": "graceful"
        }
    }


def _systemtender_update(request_data):
    if not request_data:
        return {"result": "FAILURE", "error": "Missing request data"}

    systemtender_id = request_data.get("systemtender_id")
    if not systemtender_id:
        return {"result": "FAILURE", "error": "Missing systemtender_id"}

    config = request_data.get("config")
    if not config:
        return {"result": "FAILURE", "error": "Missing config"}

    return {
        "result": "SUCCESS",
        "data": {
            "systemtender_id": systemtender_id,
            "name": "test-systemtender",
            "status": "active",
            "workers_started": 1,
            "trials_cleared": request_data.get("force", False),
            "config_history_entries": 1
        }
    }


def _systemtender_start(request_data):
    """Stub for systemtender_start - starts/resumes a systemtender"""
    systemtender_id = request_data.get("systemtender_id") if request_data else None

    if not systemtender_id:
        return {"result": "FAILURE", "error": "Missing systemtender_id parameter"}

    # Simulate start (always succeeds for stub)
    return {
        "result": "SUCCESS",
        "data": {
            "systemtender_id": systemtender_id,
            "status": "ACTIVE",
            "workers_started": 3
        }
    }


def _credentials_get(request_data):
    """Stub for credentials_get - returns list of credentials"""
    return {
        "result": "SUCCESS",
        "data": [
            {
                "id": "550e8400-e29b-41d4-a716-446655440010",
                "name": "test-credential",
                "credentialType": "ssh_private_key",
                "description": "Test SSH key",
                "windmillVariable": "f/vars/test-credential",
                "createdAt": "2024-01-01T00:00:00Z",
                "lastUsedAt": None
            },
            {
                "id": "550e8400-e29b-41d4-a716-446655440011",
                "name": "test-ssh-key",
                "credentialType": "ssh_private_key",
                "description": "Test SSH key",
                "windmillVariable": "f/vars/test-ssh-key",
                "createdAt": "2024-01-01T00:00:00Z",
                "lastUsedAt": None
            }
        ]
    }


def _credential_create(request_data):
    """Stub for credential_create - creates a new credential"""
    if not request_data:
        return {"result": "FAILURE", "error": "Missing request data"}

    name = request_data.get("name")
    if not name:
        return {"result": "FAILURE", "error": "Missing required fields: name"}

    credential_type = request_data.get("credentialType")
    if not credential_type:
        return {"result": "FAILURE", "error": "Missing required fields: credentialType"}

    # Validate credential type
    valid_types = ["ssh_private_key", "api_token", "database_connection", "http_basic_auth"]
    if credential_type not in valid_types:
        return {
            "result": "FAILURE",
            "error": f"Invalid credentialType. Must be one of: {valid_types}"
        }

    return {
        "result": "SUCCESS",
        "data": {
            "id": "550e8400-e29b-41d4-a716-446655440011",
            "name": name,
            "credentialType": credential_type,
            "description": request_data.get("description", ""),
            "windmillVariable": request_data.get("windmillVariable", ""),
            "createdAt": "2024-01-01T00:00:00Z",
            "lastUsedAt": None
        }
    }


def _credential_get(request_data):
    """Stub for credential_get - gets a specific credential"""
    credential_id = request_data.get("credentialId") if request_data else None

    # Special UUIDs for testing non-existent credentials
    if credential_id in ["00000000-0000-4000-8000-000000000000", "99999999-9999-4999-9999-999999999999"]:
        return {
            "result": "FAILURE",
            "error": f"Credential with ID '{credential_id}' not found"
        }

    # Map credential IDs to their data
    credentials = {
        "550e8400-e29b-41d4-a716-446655440010": {
            "id": "550e8400-e29b-41d4-a716-446655440010",
            "name": "test-credential",
            "credentialType": "ssh_private_key",
            "description": "Test SSH key",
            "windmillVariable": "f/vars/test-credential",
            "createdAt": "2024-01-01T00:00:00Z",
            "lastUsedAt": None
        },
        "550e8400-e29b-41d4-a716-446655440011": {
            "id": "550e8400-e29b-41d4-a716-446655440011",
            "name": "test-ssh-key",
            "credentialType": "ssh_private_key",
            "description": "Test SSH key for CI",
            "windmillVariable": "f/vars/test-ssh-key",
            "createdAt": "2024-01-01T00:00:00Z",
            "lastUsedAt": None
        }
    }

    if credential_id not in credentials:
        return {
            "result": "FAILURE",
            "error": f"Credential with ID '{credential_id}' not found"
        }

    # Return wrapped credential object
    return {
        "result": "SUCCESS",
        "data": credentials[credential_id]
    }


def _credential_delete(request_data):
    """Stub for credential_delete - deletes a credential"""
    credential_id = request_data.get("credentialId") if request_data else None

    if not credential_id:
        return {"result": "FAILURE", "error": "Missing credentialId parameter"}

    # Simulate deletion (always succeeds for stub)
    return {
        "result": "SUCCESS",
        "data": None
    }


def _targets_get(request_data):
    """Stub for targets_get - returns list of targets"""
    return {
        "result": "SUCCESS",
        "data": [
            {
                "id": "550e8400-e29b-41d4-a716-446655440020",
                "name": "test-server-1",
                "targetType": "ssh",
                "spec": {
                    "address": "10.0.5.53",
                    "username": "godon_robot",
                    "credential_id": "550e8400-e29b-41d4-a716-446655440010",
                    "allows_downtime": False
                },
                "metadata": {
                    "description": "Test SSH server"
                },
                "createdAt": "2024-01-01T00:00:00Z",
                "lastUsedAt": None
            },
            {
                "id": "550e8400-e29b-41d4-a716-446655440021",
                "name": "test-api",
                "targetType": "http",
                "spec": {
                    "url": "https://api.test.example.com",
                    "auth_type": "none"
                },
                "metadata": {
                    "description": "Test HTTP API"
                },
                "createdAt": "2024-01-01T00:00:00Z",
                "lastUsedAt": None
            }
        ]
    }


def _target_create(request_data):
    """Stub for target_create - creates a new target"""
    if not request_data:
        return {"result": "FAILURE", "error": "Missing request data"}

    name = request_data.get("name")
    if not name:
        return {"result": "FAILURE", "error": "Missing required fields: name"}

    target_type = request_data.get("targetType")
    if not target_type:
        return {"result": "FAILURE", "error": "Missing required fields: targetType"}

    valid_types = ["ssh", "http"]
    if target_type not in valid_types:
        return {
            "result": "FAILURE",
            "error": f"Invalid targetType. Must be one of: {valid_types}"
        }

    spec = request_data.get("spec")
    if not spec:
        return {"result": "FAILURE", "error": "Missing required fields: spec"}

    return {
        "result": "SUCCESS",
        "data": {
            "id": "550e8400-e29b-41d4-a716-446655440021",
            "name": name,
            "targetType": target_type,
            "spec": spec,
            "metadata": request_data.get("metadata"),
            "createdAt": "2024-01-01T00:00:00Z",
            "lastUsedAt": None
        }
    }


def _target_get(request_data):
    """Stub for target_get - gets a specific target"""
    target_id = request_data.get("targetId") if request_data else None

    if target_id in ["00000000-0000-4000-8000-000000000000", "99999999-9999-4999-9999-999999999999"]:
        return {
            "result": "FAILURE",
            "error": f"Target with ID '{target_id}' not found"
        }

    targets = {
        "550e8400-e29b-41d4-a716-446655440020": {
            "id": "550e8400-e29b-41d4-a716-446655440020",
            "name": "test-server-1",
            "targetType": "ssh",
            "spec": {
                "address": "10.0.5.53",
                "username": "godon_robot",
                "credential_id": "550e8400-e29b-41d4-a716-446655440010",
                "allows_downtime": False
            },
            "metadata": {
                "description": "Test SSH server"
            },
            "createdAt": "2024-01-01T00:00:00Z",
            "lastUsedAt": None
        },
        "550e8400-e29b-41d4-a716-446655440021": {
            "id": "550e8400-e29b-41d4-a716-446655440021",
            "name": "test-api",
            "targetType": "http",
            "spec": {
                "url": "https://api.test.example.com",
                "auth_type": "none"
            },
            "metadata": {
                "description": "Test HTTP API"
            },
            "createdAt": "2024-01-01T00:00:00Z",
            "lastUsedAt": None
        }
    }

    if target_id not in targets:
        return {
            "result": "FAILURE",
            "error": f"Target with ID '{target_id}' not found"
        }

    return {
        "result": "SUCCESS",
        "data": targets[target_id]
    }


def _target_delete(request_data):
    """Stub for target_delete - deletes a target"""
    target_id = request_data.get("targetId") if request_data else None

    if not target_id:
        return {"result": "FAILURE", "error": "Missing targetId parameter"}

    return {
        "result": "SUCCESS",
        "data": None
    }


def _steerwishes_get(request_data):
    """Stub for steerwishes_get - returns list of steerwish summaries"""
    return {
        "result": "SUCCESS",
        "data": [
            {
                "id": "550e8400-e29b-41d4-a716-446655440000",
                "state": "declared",
                "createdAt": "2026-09-10T10:30:00Z",
                "claims": [
                    {
                        "outcome": "chainend.shift",
                        "band": {"lo": -0.14, "hi": -0.06, "target": -0.10},
                    }
                ],
                "terms": [],
            }
        ],
    }


def _steerwish_create(request_data):
    """Stub for steerwish_create - the door validates the wish grammar"""
    if not request_data:
        return {"result": "FAILURE", "error": "Missing request data"}

    wish_type = request_data.get("type")
    if wish_type not in ("steer", "seek"):
        return {
            "result": "FAILURE",
            "error": "door refuses: the wish speaks its type (steer or seek, no omission)",
        }

    claims = request_data.get("claims")
    if not claims or not isinstance(claims, list):
        return {
            "result": "FAILURE",
            "error": "door refuses: declare claims (N >= 1) + terms (M >= 0) - "
            "the retired top-level outcome/band shape is refused by name",
        }

    for claim in claims:
        if not isinstance(claim, dict) or "outcome" not in claim or "band" not in claim:
            return {
                "result": "FAILURE",
                "error": "door refuses: every claim carries one outcome and one band",
            }

    terms = request_data.get("terms", [])
    if not isinstance(terms, list):
        return {"result": "FAILURE", "error": "door refuses: terms must be a list (M >= 0)"}

    return {
        "result": "SUCCESS",
        "data": {
            "id": "550e8400-e29b-41d4-a716-446655440042",
            "state": "declared",
            "createdAt": "2026-09-10T10:30:00Z",
            "claims": claims,
            "terms": terms,
            "events": [{"type": "declared", "at": "2026-09-10T10:30:00Z"}],
        },
    }


def _steerwish_get(request_data):
    """Stub for steerwish_get - returns one wish with its event history"""
    wish_id = (request_data or {}).get("wish_id")
    if not wish_id:
        return {"result": "FAILURE", "error": "Missing required field: wish_id"}

    if wish_id != "550e8400-e29b-41d4-a716-446655440000":
        return {"result": "FAILURE", "error": "steerwish {} not found".format(wish_id)}

    return {
        "result": "SUCCESS",
        "data": {
            "id": wish_id,
            "state": "acted",
            "createdAt": "2026-09-10T10:30:00Z",
            "updatedAt": "2026-09-10T11:00:00Z",
            "claims": [
                {
                    "outcome": "chainend.shift",
                    "band": {"lo": -0.14, "hi": -0.06, "target": -0.10},
                }
            ],
            "terms": [],
            "events": [
                {"type": "declared", "at": "2026-09-10T10:30:00Z"},
                {
                    "type": "planned",
                    "at": "2026-09-10T10:35:00Z",
                    "detail": "plan accepted through the door",
                },
                {"type": "acted", "at": "2026-09-10T11:00:00Z"},
            ],
        },
    }


def _steerwish_close(request_data):
    """Stub for steerwish_close - closes the wish, releases the hold"""
    wish_id = (request_data or {}).get("wish_id")
    if not wish_id:
        return {"result": "FAILURE", "error": "Missing required field: wish_id"}

    return {
        "result": "SUCCESS",
        "data": {
            "id": wish_id,
            "state": "closed",
            "createdAt": "2026-09-10T10:30:00Z",
            "updatedAt": "2026-09-10T12:45:00Z",
            "claims": [
                {
                    "outcome": "chainend.shift",
                    "band": {"lo": -0.14, "hi": -0.06, "target": -0.10},
                }
            ],
            "terms": [],
            "events": [
                {"type": "declared", "at": "2026-09-10T10:30:00Z"},
                {"type": "closed", "at": "2026-09-10T12:45:00Z"},
            ],
        },
    }


def _steerwish_update(request_data):
    """Stub for steerwish_update - the holder corrects: restated claims (and terms)"""
    data = request_data or {}

    wish_id = data.get("wish_id")
    if not wish_id:
        return {"result": "FAILURE", "error": "Missing required field: wish_id"}

    if data.get("type") != "steer":
        return {
            "result": "FAILURE",
            "error": "door refuses: corrections speak their type (steer - holds only today)",
        }

    claims = data.get("claims")
    if not claims or not isinstance(claims, list):
        return {
            "result": "FAILURE",
            "error": "door refuses: the correction restates claims (N >= 1)",
        }

    return {
        "result": "SUCCESS",
        "data": {
            "id": wish_id,
            "state": "declared",
            "createdAt": "2026-09-10T10:30:00Z",
            "updatedAt": "2026-09-10T12:00:00Z",
            "claims": claims,
            "terms": data.get("terms", []),
            "events": [
                {"type": "declared", "at": "2026-09-10T10:30:00Z"},
                {
                    "type": "corrected",
                    "at": "2026-09-10T12:00:00Z",
                    "detail": data.get("reason", "holder correction"),
                },
            ],
        },
    }


HANDLERS = {
    "systemtenders_get": _systemtenders_get,
    "systemtender_create": _systemtender_create,
    "systemtender_get": _systemtender_get,
    "systemtender_stop": _systemtender_stop,
    "systemtender_update": _systemtender_update,
    "systemtender_start": _systemtender_start,
    "credentials_get": _credentials_get,
    "credential_create": _credential_create,
    "credential_get": _credential_get,
    "credential_delete": _credential_delete,
    "targets_get": _targets_get,
    "target_create": _target_create,
    "target_get": _target_get,
    "target_delete": _target_delete,
    "steerwishes_get": _steerwishes_get,
    "steerwish_create": _steerwish_create,
    "steerwish_get": _steerwish_get,
    "steerwish_close": _steerwish_close,
    "steerwish_update": _steerwish_update,
}
