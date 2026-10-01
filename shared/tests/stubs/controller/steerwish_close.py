def main(request_data=None):
    """Stub for steerwish_close - closes the wish, releases the hold"""
    wish_id = (request_data or {}).get("wish_id")
    if not wish_id:
        return {"result": "FAILURE", "error": "Missing required field: wish_id"}

    return {
        "result": "SUCCESS",
        "data": {
            "id": wish_id,
            "state": "closed",
            "createdAt": "2026-09-10T10:30:00Z",
            "updatedAt": "2026-09-10T12:45:00Z",
            "claims": [
                {
                    "outcome": "chainend.shift",
                    "band": {"lo": -0.14, "hi": -0.06, "target": -0.10},
                }
            ],
            "terms": [],
            "events": [
                {"type": "declared", "at": "2026-09-10T10:30:00Z"},
                {"type": "closed", "at": "2026-09-10T12:45:00Z"},
            ],
        },
    }
