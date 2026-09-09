def main(request_data=None):
    """Stub for systemtender_stop - stops a systemtender's workers"""
    systemtender_id = request_data.get("systemtender_id") if request_data else None

    if not systemtender_id:
        return {"result": "FAILURE", "error": "Missing systemtender_id parameter"}

    # Simulate stop (always succeeds for stub)
    return {
        "result": "SUCCESS",
        "data": {
            "systemtender_id": systemtender_id,
            "shutdown_type": "graceful"
        }
    }
