def main(request_data=None):
    """Stub for steerwish_create - the door validates the wish grammar"""
    if not request_data:
        return {"result": "FAILURE", "error": "Missing request data"}

    claims = request_data.get("claims")
    if not claims or not isinstance(claims, list):
        return {
            "result": "FAILURE",
            "error": "door refuses: declare claims (N >= 1) + terms (M >= 0) - "
            "the retired top-level outcome/band shape is refused by name",
        }

    for claim in claims:
        if not isinstance(claim, dict) or "outcome" not in claim or "band" not in claim:
            return {
                "result": "FAILURE",
                "error": "door refuses: every claim carries one outcome and one band",
            }

    terms = request_data.get("terms", [])
    if not isinstance(terms, list):
        return {"result": "FAILURE", "error": "door refuses: terms must be a list (M >= 0)"}

    return {
        "result": "SUCCESS",
        "data": {
            "id": "550e8400-e29b-41d4-a716-446655440042",
            "state": "declared",
            "createdAt": "2026-09-10T10:30:00Z",
            "claims": claims,
            "terms": terms,
            "events": [{"type": "declared", "at": "2026-09-10T10:30:00Z"}],
        },
    }
