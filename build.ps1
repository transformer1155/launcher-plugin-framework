#!/usr/bin/env pwsh
# 便捷构建脚本：把 cargo 加进 PATH 后执行。
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
Set-Location (Split-Path -Parent $PSScriptRoot)
& cargo @args
