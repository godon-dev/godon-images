def main(request_data=None):
    """Stub for systemtender_start - starts/resumes a systemtender"""
    systemtender_id = request_data.get("systemtender_id") if request_data else None

    if not systemtender_id:
        return {"result": "FAILURE", "error": "Missing systemtender_id parameter"}

    # Simulate start (always succeeds for stub)
    return {
        "result": "SUCCESS",
        "data": {
            "systemtender_id": systemtender_id,
            "status": "ACTIVE",
            "workers_started": 3
        }
    }
