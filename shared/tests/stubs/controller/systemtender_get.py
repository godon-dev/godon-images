def main(request_data=None):
    """Stub for systemtender_get - gets a specific systemtender"""
    import json

    systemtender_id = request_data.get("systemtender_id") if request_data else None

    # Special UUIDs for testing non-existent systemtenders
    if systemtender_id in ["00000000-0000-4000-8000-000000000000", "99999999-9999-4999-9999-999999999999"]:
        return {
            "result": "FAILURE",
            "error": f"Systemtender with ID '{systemtender_id}' not found"
        }

    # For any other UUID, return wrapped systemtender object
    return {
        "result": "SUCCESS",
        "data": {
            "id": systemtender_id,
            "name": "test-systemtender",
            "status": "active",
            "createdAt": "2024-01-01T00:00:00Z",
            "config": {
                "type": "linux_performance"
            }
        }
    }
