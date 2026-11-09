# Usage: powershell -File packaging/windows/with-env.ps1 <command> [args...]
# Runs the command with the Qt/Kirigami environment from env.ps1 and propagates its exit code.
. "$PSScriptRoot\env.ps1"
if ($args.Count -eq 0) { throw 'with-env.ps1: no command given' }
& $args[0] @($args | Select-Object -Skip 1)
exit $LASTEXITCODE
