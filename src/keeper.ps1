# Holds Windows awake for exactly as long as this process lives.
#
# t3-keep-awake starts it from WSL and writes one line per heartbeat to
# stdin. The hold ends when stdin closes (the daemon released it, exited, or
# was killed) or when no line arrives within the timeout (the daemon or WSL
# hung). Process exit alone clears the request too; the explicit release is
# belt and braces.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

Add-Type -Namespace T3KeepAwake -Name Power -MemberDefinition '[DllImport("kernel32.dll")] public static extern uint SetThreadExecutionState(uint esFlags);'

# Windows PowerShell 5.1 reads hex literals of 0x80000000 and above as negative
# Int32 values, which the uint parameter rejects, so the flags are decimal.
$hold = [uint32]__HOLD_FLAGS__
$release = [uint32]__RELEASE_FLAGS__
$timeoutMs = __TIMEOUT_MS__

# [Console]::In.ReadLineAsync() blocks on 5.1, which would defeat the timeout.
# A StreamReader over the raw stream reads asynchronously.
$stdin = New-Object IO.StreamReader([Console]::OpenStandardInput())
$stdout = [Console]::Out

function Say($line) {
    $stdout.WriteLine($line)
    $stdout.Flush()
}

function Release($why) {
    [T3KeepAwake.Power]::SetThreadExecutionState($release) | Out-Null
    Say "released $why"
    exit 0
}

if ([T3KeepAwake.Power]::SetThreadExecutionState($hold) -eq 0) {
    Say 'error SetThreadExecutionState failed'
    exit 1
}
Say "holding $PID"

while ($true) {
    $read = $stdin.ReadLineAsync()
    if (-not $read.Wait($timeoutMs)) { Release 'heartbeat-timeout' }
    if ($null -eq $read.Result) { Release 'stdin-closed' }
    Say 'pong'
}
