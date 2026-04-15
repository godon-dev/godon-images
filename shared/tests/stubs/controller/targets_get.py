def main(request_data=None):
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
