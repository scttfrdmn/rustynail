#!/bin/bash
# Quick test script for health endpoints
# Note: Requires RustyNail to be running

PORT=${1:-8080}
BASE_URL="http://localhost:$PORT"

echo "Testing RustyNail Health Endpoints"
echo "===================================="
echo ""

echo "1. Testing /health endpoint..."
curl -s "$BASE_URL/health" | jq '.' || echo "Failed"
echo ""

echo "2. Testing /status endpoint..."
curl -s "$BASE_URL/status" | jq '.' || echo "Failed"
echo ""

echo "3. Testing /metrics endpoint..."
curl -s "$BASE_URL/metrics" | jq '.' || echo "Failed"
echo ""

echo "4. Testing /ready endpoint..."
curl -s "$BASE_URL/ready" | jq '.' || echo "Failed"
echo ""

echo "5. Testing /live endpoint..."
curl -s "$BASE_URL/live" | jq '.' || echo "Failed"
echo ""

echo "Done!"
