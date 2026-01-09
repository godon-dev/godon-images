def main(credential_data):
    """Stub for credential_create - creates a new credential"""
    return {
        "id": "550e8400-e29b-41d4-a716-446655440011",
        "name": credential_data.get("name", "new-credential"),
        "credentialType": credential_data.get("credentialType", "ssh_private_key"),
        "description": credential_data.get("description", ""),
        "windmillVariable": credential_data.get("windmillVariable", ""),
        "createdAt": "2024-01-01T00:00:00Z",
        "lastUsedAt": None
    }
