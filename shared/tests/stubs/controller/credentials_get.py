def main():
    """Stub for credentials_get - returns list of credentials"""
    return {
        "credentials": [
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
                "description": "Test SSH key for CI",
                "windmillVariable": "f/vars/test-ssh-key",
                "createdAt": "2024-01-01T00:00:00Z",
                "lastUsedAt": None
            }
        ]
    }
