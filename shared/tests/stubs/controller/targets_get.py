def main(request_data=None):
    """Stub for targets_get - returns list of targets"""
    return {
        "result": "SUCCESS",
        "data": [
            {
                "id": "550e8400-e29b-41d4-a716-446655440020",
                "name": "test-server-1",
                "targetType": "ssh",
                "address": "10.0.5.53",
                "username": "godon_robot",
                "credentialId": "550e8400-e29b-41d4-a716-446655440010",
                "credentialName": "test-credential",
                "description": "Test SSH server",
                "allowsDowntime": False,
                "createdAt": "2024-01-01T00:00:00Z",
                "lastUsedAt": None
            },
            {
                "id": "550e8400-e29b-41d4-a716-446655440021",
                "name": "test-api",
                "targetType": "http",
                "address": "https://api.test.example.com",
                "username": None,
                "credentialId": None,
                "credentialName": None,
                "description": "Test HTTP API",
                "allowsDowntime": True,
                "createdAt": "2024-01-01T00:00:00Z",
                "lastUsedAt": None
            }
        ]
    }
