# Prints Windows' system-wide execution state: the union of every process's
# SetThreadExecutionState request. Unlike `powercfg /requests`, it needs no
# elevation.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

Add-Type -Namespace HerdrKeepAwake -Name Probe -MemberDefinition '[DllImport("powrprof.dll")] public static extern uint CallNtPowerInformation(int level, IntPtr inBuf, uint inLen, out uint outBuf, uint outLen);'

$state = [uint32]0
# 16 = SystemExecutionState
$rc = [HerdrKeepAwake.Probe]::CallNtPowerInformation(16, [IntPtr]::Zero, 0, [ref]$state, 4)
[Console]::Out.WriteLine("state $rc $state")
