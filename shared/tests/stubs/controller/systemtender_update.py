def main(request_data=None):
    if not request_data:
        return {"result": "FAILURE", "error": "Missing request data"}

    systemtender_id = request_data.get("systemtender_id")
    if not systemtender_id:
        return {"result": "FAILURE", "error": "Missing systemtender_id"}

    config = request_data.get("config")
    if not config:
        return {"result": "FAILURE", "error": "Missing config"}

    return {
        "result": "SUCCESS",
        "data": {
            "systemtender_id": systemtender_id,
            "name": "test-systemtender",
            "status": "active",
            "workers_started": 1,
            "trials_cleared": request_data.get("force", False),
            "config_history_entries": 1
        }
    }
