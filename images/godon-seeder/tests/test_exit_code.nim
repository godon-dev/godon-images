import std/[logging, json]

# Mock types for testing exit code behavior
type
  ScriptSettings = object
    summary*: string
    description*: string

  MockWindmillClient = object
    shouldFail*: bool
    deployCallCount*: int
    lastError*: string

proc newMockWindmillClient(shouldFail: bool = false): MockWindmillClient =
  MockWindmillClient(
    shouldFail: shouldFail,
    deployCallCount: 0,
    lastError: ""
  )

proc deployScript*(client: var MockWindmillClient, workspace: string, scriptPath: string,
                   content: string, settings: ScriptSettings) =
  ## Mock deployment that can fail or succeed
  inc(client.deployCallCount)

  if client.shouldFail:
    let errorMsg = "Failed to deploy script: " & scriptPath
    client.lastError = errorMsg
    error(errorMsg)
    raise newException(ValueError, errorMsg)

  info("✅ Successfully deployed script: " & scriptPath)

proc deployScriptWithExitCodeCheck(client: var MockWindmillClient, workspace: string,
                                    scriptPath: string, content: string, settings: ScriptSettings): int =
  ## Simulates deployScriptWithRetry behavior - returns 0 on success, 1 on failure
  try:
    client.deployScript(workspace, scriptPath, content, settings)
    return 0
  except CatchableError as e:
    logging.error("Failed to deploy script " & scriptPath & " after retries: " & e.msg)
    return 1

when isMainModule:
  addHandler(newConsoleLogger())
  setLogFilter(lvlInfo)

  echo "🧪 Running exit code unit tests..."
  echo ""

  # Test 1: Successful deployment returns 0
  echo "  Test 1: Successful deployment returns exit code 0..."
  var successClient = newMockWindmillClient(shouldFail = false)
  let exitCode1 = deployScriptWithExitCodeCheck(successClient, "test-workspace", "f/test/script", "content", ScriptSettings(summary: "test", description: "test"))
  assert exitCode1 == 0, "Expected exit code 0, got " & $exitCode1
  assert successClient.deployCallCount == 1, "Expected 1 deploy call"
  echo "  ✅ Passed: Exit code 0 on success"
  echo ""

  # Test 2: Failed deployment returns 1
  echo "  Test 2: Failed deployment returns exit code 1..."
  var failClient = newMockWindmillClient(shouldFail = true)
  let exitCode2 = deployScriptWithExitCodeCheck(failClient, "test-workspace", "f/test/fail", "content", ScriptSettings(summary: "test", description: "test"))
  assert exitCode2 == 1, "Expected exit code 1, got " & $exitCode2
  assert failClient.deployCallCount == 1, "Expected 1 deploy call"
  assert failClient.lastError != "", "Expected error message to be set"
  echo "  ✅ Passed: Exit code 1 on failure"
  echo "  ✅ Passed: Error was logged: " & failClient.lastError
  echo ""

  # Test 3: Multiple deployments, some fail
  echo "  Test 3: Multiple deployments with mixed results..."
  var mixedClient = newMockWindmillClient(shouldFail = false)
  var totalFailures = 0

  # First deployment succeeds
  totalFailures += deployScriptWithExitCodeCheck(mixedClient, "test", "f/test/success1", "content", ScriptSettings(summary: "test", description: "test"))
  # Second deployment succeeds
  totalFailures += deployScriptWithExitCodeCheck(mixedClient, "test", "f/test/success2", "content", ScriptSettings(summary: "test", description: "test"))
  # Third deployment fails
  mixedClient.shouldFail = true
  totalFailures += deployScriptWithExitCodeCheck(mixedClient, "test", "f/test/fail", "content", ScriptSettings(summary: "test", description: "test"))

  assert totalFailures == 1, "Expected 1 failure, got " & $totalFailures
  assert mixedClient.deployCallCount == 3, "Expected 3 deploy calls"
  echo "  ✅ Passed: Correct failure count (1 failure out of 3 deployments)"
  echo ""

  # Test 4: All deployments fail
  echo "  Test 4: All deployments fail..."
  var allFailClient = newMockWindmillClient(shouldFail = true)
  var allFailures = 0

  allFailures += deployScriptWithExitCodeCheck(allFailClient, "test", "f/test/fail1", "content", ScriptSettings(summary: "test", description: "test"))
  allFailures += deployScriptWithExitCodeCheck(allFailClient, "test", "f/test/fail2", "content", ScriptSettings(summary: "test", description: "test"))

  assert allFailures == 2, "Expected 2 failures, got " & $allFailures
  echo "  ✅ Passed: Correct failure count (all 2 deployments failed)"
  echo ""

  echo "✅ All exit code tests passed!"
  echo ""
  echo "Summary:"
  echo "  - Successful deployments return exit code 0"
  echo "  - Failed deployments return exit code 1"
  echo "  - Multiple deployments accumulate failure counts correctly"
  echo "  - Errors are properly logged on failure"
