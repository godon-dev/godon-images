def main(request_data=None):
    """Stub for breeder_get - gets a specific breeder"""
    import json

    breeder_id = request_data.get("breeder_id") if request_data else None

    # Special UUIDs for testing non-existent breeders
    if breeder_id in ["00000000-0000-4000-8000-000000000000", "99999999-9999-4999-9999-999999999999"]:
        return {
            "result": "FAILURE",
            "error": f"Breeder with ID '{breeder_id}' not found"
        }

    # For any other UUID, return breeder object directly (controller unwraps service response)
    return {
        "id": breeder_id,
        "name": "test-breeder",
        "status": "active",
        "createdAt": "2024-01-01T00:00:00Z",
        "config": {
            "type": "linux_performance"
        }
    }
