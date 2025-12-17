# Express Proxy for Windmill API Testing

## Why We Use This Proxy

We use an Express proxy because **Prism alone cannot provide conditional routing for generic execution endpoints**. 

**The Problem:**
- Windmill uses a single endpoint: `/w/{workspace}/jobs/run_wait_result/p/{path}`
- The `{path}` parameter contains different flow names (breeders_get, breeder_create, etc.)
- Prism returns the same generic JSON schema for all requests
- We need different mock responses for each flow

**Our Solution:**
The proxy:
1. **Validates requests with Prism first** (auth, JSON structure, etc.)
2. **Transforms only specific flow responses** to realistic mock data
3. **Passes through all errors transparently** so godon-api sees real validation failures

**Result:** godon-cli gets proper mock responses while maintaining all Prism validation behavior.