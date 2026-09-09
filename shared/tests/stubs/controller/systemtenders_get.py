def main(request_data=None):
    """Stub for systemtenders_get - returns list of systemtenders"""
    return {
        "result": "SUCCESS",
        "data": [
            {
                "id": "test-systemtender-1",
                "name": "test-systemtender",
                "status": "active",
                "createdAt": "2024-01-01T00:00:00Z"
            }
        ]
    }
