def main(request_data=None):
    if not request_data:
        return {"result": "FAILURE", "error": "Missing request data"}

    breeder_id = request_data.get("breeder_id")
    if not breeder_id:
        return {"result": "FAILURE", "error": "Missing breeder_id"}

    config = request_data.get("config")
    if not config:
        return {"result": "FAILURE", "error": "Missing config"}

    return {
        "result": "SUCCESS",
        "data": {
            "breeder_id": breeder_id,
            "name": "test-breeder",
            "status": "active",
            "workers_started": 1,
            "trials_cleared": request_data.get("force", False),
            "config_history_entries": 1
        }
    }
