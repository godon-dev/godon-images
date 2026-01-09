def main(breeder_id):
    """Stub for breeder_get - gets a specific breeder"""
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
