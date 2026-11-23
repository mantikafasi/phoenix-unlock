<#
.SYNOPSIS
Fast build, patch, install, hot-reload, and live debug CLI for Phoenix Unlock.

.EXAMPLE
.\scripts\dev.ps1                     # Fast dev-fast Rust build + patch + install + launch
.\scripts\dev.ps1 -Release            # Release build + update web/plugins/libphoenix-arm64.so + install
.\scripts\dev.ps1 -SkipBuild          # Bundle plugins + patch + install (no Rust compile)
.\scripts\dev.ps1 -HotReload taptap   # Bundle & hot-reload one plugin over ADB (<0.5s)
.\scripts\dev.ps1 -Logs               # Print live logs from 127.0.0.1:4242
.\scripts\dev.ps1 -Ops banner         # Inspect recent decoded UI-op batches matching "banner"
.\scripts\dev.ps1 -Events view        # Inspect recent view/host events
.\scripts\dev.ps1 -Tree               # Dump live Phoenix view tree from root
.\scripts\dev.ps1 -Eval "phoenix.ui.root()"
.\scripts\dev.ps1 -Test               # Run fast desktop Rust unit tests (phoenix-ui, phoenix-wire)
#>
[CmdletBinding()]
param(
   [switch]$Release,
   [switch]$SkipBuild,
   [switch]$NoInstall,
   [switch]$NoLaunch,
   [switch]$Test,
   [string]$HotReload,
   [string]$Eval,
   [switch]$Logs,
   [switch]$Ops,
   [switch]$Events,
   [switch]$Tree,
   [switch]$Clear,
   [Parameter(Position = 0)]
   [string]$Arg
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "E:\phoenix-build\target" }
$OutApk = if ($env:PHOENIX_OUT_APK) { $env:PHOENIX_OUT_APK } else { "E:\phoenix-build\phoenix-updated.apk" }
$SplitDir = if ($env:PHOENIX_SPLITS) { $env:PHOENIX_SPLITS } else { Join-Path (Split-Path $RepoRoot -Parent) "arm64\dl" }

function Find-Esbuild {
   if ($env:ESBUILD -and (Test-Path $env:ESBUILD)) { return $env:ESBUILD }
   $candidates = @(
      "$env:APPDATA\npm\node_modules\wrangler\node_modules\@esbuild\win32-x64\esbuild.exe",
      "$env:APPDATA\npm\node_modules\esbuild\esbuild.exe"
   )
   foreach ($c in $candidates) {
      if (Test-Path $c) { return $c }
   }
   $cmd = Get-Command esbuild -ErrorAction SilentlyContinue
   if ($cmd) { return $cmd.Source }
   throw "esbuild not found; set `$env:ESBUILD"
}

function Forward-DebugShell {
   adb start-server | Out-Null
   adb forward tcp:4242 tcp:4242 | Out-Null
}

function Invoke-DebugGet([string]$Path) {
   Forward-DebugShell
   Invoke-RestMethod -Uri "http://127.0.0.1:4242$Path"
}

function Invoke-DebugPost([string]$Path, [string]$Body = "", [string]$ContentType = "text/plain") {
   Forward-DebugShell
   Invoke-RestMethod -Uri "http://127.0.0.1:4242$Path" -Method Post -Body $Body -ContentType $ContentType
}

# Fast host unit tests
if ($Test) {
   Push-Location $RepoRoot
   try {
      $env:CARGO_TARGET_DIR = $TargetDir
      cargo +stable-x86_64-pc-windows-gnu test -p phoenix-ui -p phoenix-wire
      exit $LASTEXITCODE
   } finally {
      Pop-Location
   }
}

# Remote debug shell commands
if ($Clear) {
   Invoke-DebugPost "/clear" | ConvertTo-Json -Depth 4
   exit 0
}
if ($Logs) {
   Invoke-DebugGet "/logs"
   exit 0
}
if ($Eval) {
   Invoke-DebugPost "/eval" $Eval
   exit 0
}
if ($Ops) {
   $q = if ($Arg) { "?match=$([uri]::EscapeDataString($Arg))" } else { "" }
   Invoke-DebugGet "/ops$q" | ConvertTo-Json -Depth 12
   exit 0
}
if ($Events) {
   $q = if ($Arg) { "?match=$([uri]::EscapeDataString($Arg))" } else { "" }
   Invoke-DebugGet "/events$q" | ConvertTo-Json -Depth 12
   exit 0
}
if ($Tree) {
   $p = if ($Arg) { "/tree/$Arg" } else { "/tree" }
   Invoke-DebugGet $p | ConvertTo-Json -Depth 16
   exit 0
}

# Sub-second plugin hot-reload via Remote Debug Shell
if ($HotReload) {
   $entry = Join-Path $RepoRoot "plugins\$HotReload\src\index.ts"
   if (-not (Test-Path $entry)) { throw "Plugin entry not found: $entry" }
   $esbuild = Find-Esbuild
   $tmp = Join-Path ([System.IO.Path]::GetTempPath()) "phoenix-hot-$HotReload.js"
   try {
      $headerLines = Get-Content $entry | ForEach-Object { $_.Trim() } | Where-Object { $_ -ne "" }
      $banner = @()
      foreach ($line in $headerLines) {
         if ($line.StartsWith("//")) { $banner += $line } else { break }
      }
      & $esbuild $entry --bundle --format=iife --target=es2020 --outfile=$tmp
      if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
      $body = [System.IO.File]::ReadAllText($tmp)
      if ($banner.Count -gt 0) {
         $body = ($banner -join "`n") + "`n" + $body
      }
      Forward-DebugShell
      $resp = Invoke-RestMethod -Uri "http://127.0.0.1:4242/plugins/$HotReload" -Method Put -Body ([System.Text.Encoding]::UTF8.GetBytes($body)) -ContentType "text/javascript; charset=utf-8"
      Write-Host "Hot-reloaded $HotReload`: $($resp | ConvertTo-Json -Compress)"
   } finally {
      Remove-Item $tmp -Force -ErrorAction SilentlyContinue
   }
   exit 0
}

# Build + bundle + patch + install loop
Push-Location $RepoRoot
try {
   $profileName = if ($Release) { "release" } else { "dev-fast" }
   $soPath = Join-Path $TargetDir "aarch64-linux-android\$profileName\libphoenix.so"

   if (-not $SkipBuild) {
      $ndk = if ($env:ANDROID_NDK_HOME -and (Test-Path $env:ANDROID_NDK_HOME)) {
         $env:ANDROID_NDK_HOME
      } elseif (Test-Path "C:/Users/manti/AppData/Local/Android/Sdk/ndk/26.0.10792818") {
         "C:/Users/manti/AppData/Local/Android/Sdk/ndk/26.0.10792818"
      } else {
         (Get-ChildItem "$env:LOCALAPPDATA\Android\Sdk\ndk" | Sort-Object Name -Descending | Select-Object -First 1).FullName.Replace('\', '/')
      }
      $clangVer = (Get-ChildItem "$ndk/toolchains/llvm/prebuilt/windows-x86_64/lib/clang" | Select-Object -First 1).Name
      $env:ANDROID_NDK_HOME = $ndk
      if (-not $env:SHADOWHOOK_SRC) {
         $env:SHADOWHOOK_SRC = "C:/Users/manti/AppData/Local/Temp/claude/b272d34725fa8361/6b7c63e6-8e69-4925-9a97-7ee1606662df/scratchpad/rust/android-inline-hook/shadowhook/src/main/cpp"
      }
      if (-not $env:BYTEHOOK_SRC) {
         $env:BYTEHOOK_SRC = "C:/Users/manti/AppData/Local/Temp/claude/b272d34725fa8361/6b7c63e6-8e69-4925-9a97-7ee1606662df/scratchpad/rust/bhook/bytehook/src/main/cpp"
      }
      $env:CARGO_TARGET_DIR = $TargetDir
      if (-not $env:LIBCLANG_PATH) { $env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin" }
      [Environment]::SetEnvironmentVariable(
         "BINDGEN_EXTRA_CLANG_ARGS_aarch64-linux-android",
         "--target=aarch64-linux-android28 --sysroot=$ndk/toolchains/llvm/prebuilt/windows-x86_64/sysroot -isystem $ndk/toolchains/llvm/prebuilt/windows-x86_64/lib/clang/$clangVer/include",
         "Process"
      )
      $cleanPath = ($env:PATH -split ';' | Where-Object { $_ -notmatch 'mingw' }) -join ';'
      if (Test-Path "E:\phoenix-build\bin") {
         $env:PATH = "E:\phoenix-build\bin;$cleanPath"
      } else {
         $env:PATH = $cleanPath
      }

      Write-Host "Building phoenix-loader (arm64-v8a, profile=$profileName)..."
      $cargoArgs = @("+stable-x86_64-pc-windows-gnu", "ndk", "-t", "arm64-v8a", "-P", "28", "--link-builtins", "build", "-p", "phoenix-loader")
      if ($Release) { $cargoArgs += "--release" } else { $cargoArgs += @("--profile", "dev-fast") }
      & cargo @cargoArgs
      if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

      if ($Release) {
         Copy-Item $soPath (Join-Path $RepoRoot "web\plugins\libphoenix-arm64.so") -Force
         Write-Host "Updated web/plugins/libphoenix-arm64.so"
      }
   }

   $env:ESBUILD = Find-Esbuild
   node web/tools/bundle-plugins.mjs
   if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

   if (-not $Release -and (Test-Path $soPath)) {
      $env:PHOENIX_LOADER = $soPath
   } else {
      Remove-Item Env:\PHOENIX_LOADER -ErrorAction SilentlyContinue
   }

   $splits = @(
      (Join-Path $SplitDir "base.apk"),
      (Join-Path $SplitDir "config.arm64_v8a.apk"),
      (Join-Path $SplitDir "config.xxhdpi.apk"),
      (Join-Path $SplitDir "config.en.apk")
   ) | Where-Object { Test-Path $_ }
   node web/tools/node-build.mjs $OutApk @splits
   if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

   if (-not $NoInstall) {
      adb start-server | Out-Null
      adb install -r $OutApk
      if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
      if (-not $NoLaunch) {
         adb shell am force-stop com.discord
         adb shell monkey -p com.discord -c android.intent.category.LAUNCHER 1 | Out-Null
      }
   }
} finally {
   Remove-Item Env:\PHOENIX_LOADER -ErrorAction SilentlyContinue
   Pop-Location
}
