#!/bin/bash
# Deploy stub scripts to Windmill for testing
# This script directly uses the Windmill API - no seeder dependency

set -e

WINDMILL_URL="${WINDMILL_URL:-http://localhost:9101}"
WORKSPACE="${WORKSPACE:-test}"
STUBS_DIR="${STUBS_DIR:-$(dirname "$0")/stubs}"

echo "🚀 Deploying stub scripts to Windmill..."
echo "   Windmill URL: $WINDMILL_URL"
echo "   Workspace: $WORKSPACE"
echo "   Stubs directory: $STUBS_DIR"
echo ""

# Login and get token
echo "🔐 Authenticating..."
TOKEN=$(curl -s -X POST "$WINDMILL_URL/api/auth/login" \
  -H "Content-Type: application/json" \
  -d '{"email":"admin@windmill.dev","password":"changeme"}' | tr -d '"')

if [ -z "$TOKEN" ] || [ "$TOKEN" == "null" ]; then
  echo "❌ Failed to authenticate"
  exit 1
fi

echo "✅ Authenticated successfully"
echo ""

# Create workspace
echo "📁 Creating workspace: $WORKSPACE"
curl -s -X POST "$WINDMILL_URL/api/workspaces/create" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d "{\"id\": \"$WORKSPACE\", \"name\": \"$WORKSPACE\"}" > /dev/null 2>&1 || true
echo "✅ Workspace ready"
echo ""

# Create folder
echo "📂 Creating folder: controller"
curl -s -X POST "$WINDMILL_URL/api/w/$WORKSPACE/folders/create" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name": "controller"}' > /dev/null 2>&1 || true
echo "✅ Folder ready"
echo ""

# Deploy each stub script
for script in "$STUBS_DIR"/controller/*.py; do
  if [ -f "$script" ]; then
    script_name=$(basename "$script" .py)
    script_path="f/controller/$script_name"

    echo "📝 Deploying: $script_path"

    # Read script content
    content=$(cat "$script")

    # Deploy script
    result=$(curl -s -X POST "$WINDMILL_URL/api/w/$WORKSPACE/scripts/create" \
      -H "Authorization: Bearer $TOKEN" \
      -H "Content-Type: application/json" \
      -d "{
        \"path\": \"$script_path\",
        \"content\": $(echo "$content" | jq -Rs .),
        \"language\": \"python3\",
        \"summary\": \"Test stub\",
        \"description\": \"Stub script for API testing\"
      }")

    if echo "$result" | grep -qE "error|Error"; then
      echo "   ❌ Failed: $result"
      exit 1
    else
      echo "   ✅ Success: ${result:0:20}..."
    fi

    # Small delay to avoid overwhelming Windmill
    sleep 0.5
  fi
done

echo ""
echo "🎉 All stub scripts deployed!"
echo ""
echo "📊 Summary:"
echo "   Workspace: $WORKSPACE"
echo "   Scripts deployed: $(ls -1 "$STUBS_DIR"/controller/*.py 2>/dev/null | wc -l)"
