Set-Location $PSScriptRoot
if ($env:FAST -eq "1") { $env:LIVEKIT_BUNDLE_SKIP = "1" }
# Regenerate Tailwind CSS if the CLI is available (warn if not).
if (Get-Command npx -ErrorAction SilentlyContinue) {
    Push-Location client
    npx @tailwindcss/cli -i assets/tailwind.css -o assets/tailwind.out.css --minify
    Pop-Location
} else {
    # ASCII only: Windows PowerShell 5.1 reads a BOM-less script as ANSI, so a
    # UTF-8 em dash here breaks the string and the whole file fails to parse.
    Write-Warning "npx not found - using committed tailwind.out.css. Install Node.js to regenerate."
}
cargo run -p dioxusfun
