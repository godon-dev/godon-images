def main(request_data=None):
    """Stub for steerwishes_get - returns list of steerwish summaries"""
    return {
        "result": "SUCCESS",
        "data": [
            {
                "id": "550e8400-e29b-41d4-a716-446655440000",
                "state": "declared",
                "createdAt": "2026-09-10T10:30:00Z",
                "claims": [
                    {
                        "outcome": "chainend.shift",
                        "band": {"lo": -0.14, "hi": -0.06, "target": -0.10},
                    }
                ],
                "terms": [],
            }
        ],
    }
