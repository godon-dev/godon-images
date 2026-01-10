def main(request_data=None):
    """Stub for breeder_create - creates a new breeder"""
    return {
        "id": "test-breeder-2",
        "name": request_data.get("name", "new-breeder"),
        "status": "active",
        "createdAt": "2024-01-01T00:00:00Z"
    }
