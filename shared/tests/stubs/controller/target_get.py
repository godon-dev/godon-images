def main(request_data=None):
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
