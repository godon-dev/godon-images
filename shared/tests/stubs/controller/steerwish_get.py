def main(request_data=None):
    """Stub for steerwish_get - returns one wish with its event history"""
    wish_id = (request_data or {}).get("wish_id")
    if not wish_id:
        return {"result": "FAILURE", "error": "Missing required field: wish_id"}

    if wish_id != "550e8400-e29b-41d4-a716-446655440000":
        return {"result": "FAILURE", "error": "steerwish {} not found".format(wish_id)}

    return {
        "result": "SUCCESS",
        "data": {
            "id": wish_id,
            "state": "acted",
            "createdAt": "2026-09-10T10:30:00Z",
            "updatedAt": "2026-09-10T11:00:00Z",
            "claims": [
                {
                    "outcome": "chainend.shift",
                    "band": {"lo": -0.14, "hi": -0.06, "target": -0.10},
                }
            ],
            "terms": [],
            "events": [
                {"type": "declared", "at": "2026-09-10T10:30:00Z"},
                {
                    "type": "planned",
                    "at": "2026-09-10T10:35:00Z",
                    "detail": "plan accepted through the door",
                },
                {"type": "acted", "at": "2026-09-10T11:00:00Z"},
            ],
        },
    }
