def main(request_data=None):
    """Stub for credential_get - gets a specific credential"""
    credential_id = request_data.get("credentialId") if request_data else None

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

    credential_data = credentials.get(credential_id, {
        "id": credential_id,
        "name": "unknown",
        "credentialType": "ssh_private_key",
        "description": "Unknown credential",
        "windmillVariable": "",
        "createdAt": "2024-01-01T00:00:00Z",
        "lastUsedAt": None
    })

    # Return in the same format as the controller
    return {
        "result": "SUCCESS",
        "credential": credential_data
    }
