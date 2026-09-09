def main(request_data=None):
    """Stub for breeder_create - creates a new breeder"""
    if not request_data:
        return {"result": "FAILURE", "error": "Missing request data"}

    name = request_data.get("name")
    if not name:
        return {"result": "FAILURE", "error": "Missing required field: name"}

    # Validate name format (no spaces)
    if " " in name:
        return {"result": "FAILURE", "error": "Invalid name format: spaces not allowed"}

    config = request_data.get("config")
    if not config:
        return {"result": "FAILURE", "error": "Missing required field: config"}

    return {
        "result": "SUCCESS",
        "data": {
            "id": "test-breeder-2",
            "name": name,
            "status": "active",
            "createdAt": "2024-01-01T00:00:00Z"
        }
    }
