def main(request_data=None):
    """Stub for credential_create - creates a new credential"""
    return {
        "id": "550e8400-e29b-41d4-a716-446655440011",
        "name": request_data.get("name", "new-credential"),
        "credentialType": request_data.get("credentialType", "ssh_private_key"),
        "description": request_data.get("description", ""),
        "windmillVariable": request_data.get("windmillVariable", ""),
        "createdAt": "2024-01-01T00:00:00Z",
        "lastUsedAt": None
    }
