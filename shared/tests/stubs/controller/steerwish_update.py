def main(request_data=None):
    """Stub for steerwish_update - the holder corrects: restated claims (and terms)"""
    data = request_data or {}

    wish_id = data.get("wish_id")
    if not wish_id:
        return {"result": "FAILURE", "error": "Missing required field: wish_id"}

    claims = data.get("claims")
    if not claims or not isinstance(claims, list):
        return {
            "result": "FAILURE",
            "error": "door refuses: the correction restates claims (N >= 1)",
        }

    return {
        "result": "SUCCESS",
        "data": {
            "id": wish_id,
            "state": "declared",
            "createdAt": "2026-09-10T10:30:00Z",
            "updatedAt": "2026-09-10T12:00:00Z",
            "claims": claims,
            "terms": data.get("terms", []),
            "events": [
                {"type": "declared", "at": "2026-09-10T10:30:00Z"},
                {
                    "type": "corrected",
                    "at": "2026-09-10T12:00:00Z",
                    "detail": data.get("reason", "holder correction"),
                },
            ],
        },
    }
