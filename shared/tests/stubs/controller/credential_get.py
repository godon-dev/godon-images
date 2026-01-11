def main(request_data=None):
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
