def main(verb=None, request_data=None):
    """Stub for the controller's slow lane entry - the verb rides the
    arguments; every handler below is the per-verb stub body, verbatim."""
    handler = HANDLERS.get(verb)
    if handler is None:
        return {"result": "FAILURE", "error": f"unknown verb: {verb}"}
    return handler(request_data)


def _systemtender_delete(request_data):
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


def _steerwish_delete(request_data):
    """Stub for steerwish_delete - closes first, then forgets"""
    wish_id = (request_data or {}).get("wish_id")
    if not wish_id:
        return {"result": "FAILURE", "error": "Missing required field: wish_id"}

    return {
        "result": "SUCCESS",
        "data": {"deleted": True, "id": wish_id},
    }


HANDLERS = {
    "systemtender_delete": _systemtender_delete,
    "steerwish_delete": _steerwish_delete,
}
