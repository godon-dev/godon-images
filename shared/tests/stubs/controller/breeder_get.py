def main(request_data=None):
    """Stub for breeder_get - gets a specific breeder"""
    breeder_id = request_data.get("breederId") if request_data else None
    return {
        "id": breeder_id,
        "name": "test-breeder",
        "status": "active",
        "createdAt": "2024-01-01T00:00:00Z",
        "config": {
            "step_size": 200,
            "max_iterations": 10
        }
    }
