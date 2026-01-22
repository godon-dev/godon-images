def main(request_data=None):
    """Stub for breeder_stop - stops a breeder's workers"""
    breeder_id = request_data.get("breeder_id") if request_data else None

    if not breeder_id:
        return {"result": "FAILURE", "error": "Missing breeder_id parameter"}

    # Simulate stop (always succeeds for stub)
    return {
        "result": "SUCCESS",
        "data": {
            "breeder_id": breeder_id,
            "shutdown_type": "graceful"
        }
    }
