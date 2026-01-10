def main(request_data=None):
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
