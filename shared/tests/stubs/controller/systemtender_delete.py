def main(request_data=None):
    """Stub for systemtender_delete - deletes a systemtender"""
    systemtender_id = request_data.get("systemtender_id") if request_data else None
    force = request_data.get("force", False) if request_data else False

    if not systemtender_id:
        return {"result": "FAILURE", "error": "Missing systemtender_id parameter"}

    # Simulate deletion (always succeeds for stub)
    return {
        "result": "SUCCESS",
        "data": {
            "systemtender_id": systemtender_id,
            "delete_type": "force" if force else "graceful",
            "workers_cancelled": 1 if force else 0
        }
    }
