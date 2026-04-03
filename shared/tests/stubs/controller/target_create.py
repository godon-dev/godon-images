def main(request_data=None):
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

    address = request_data.get("address")
    if not address:
        return {"result": "FAILURE", "error": "Missing required fields: address"}

    return {
        "result": "SUCCESS",
        "data": {
            "id": "550e8400-e29b-41d4-a716-446655440021",
            "name": name,
            "targetType": target_type,
            "address": address,
            "username": request_data.get("username"),
            "credentialId": request_data.get("credentialId"),
            "credentialName": request_data.get("credentialName"),
            "description": request_data.get("description", ""),
            "allowsDowntime": request_data.get("allowsDowntime", False),
            "createdAt": "2024-01-01T00:00:00Z",
            "lastUsedAt": None
        }
    }
