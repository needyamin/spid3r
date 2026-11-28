# Spid3r — Microsoft Store MSIX Packaging

This directory holds the Microsoft Store MSIX manifest template (`AppxManifest.xml.in`) and Store tile/logo assets (`Assets/`) for **Spid3r** (`Store ID: 9NPH5XX0F6KF`).

## Configured Partner Center Identity

| Field | Default Value | Override Env / CLI Flag |
| :--- | :--- | :--- |
| `Package/Identity/Name` | `ANSNEWTECH.Spid3r` | `MSIX_IDENTITY_NAME` / `--msix-identity` |
| `Package/Identity/Publisher` | `CN=087A9974-75CB-44FC-B893-8D3999E5E5E5` | `MSIX_PUBLISHER` / `--msix-publisher` |
| `PublisherDisplayName` | `ANSNEW TECH.` | `MSIX_PUBLISHER_DISPLAY_NAME` / `--msix-publisher-display` |
| `Version` | `<Cargo.toml X.Y.Z>.0` (e.g. `1.5.1.0`) | Derived automatically |

## What the `.msix` Contains

- `spid3r.exe`, `sp.exe`, `yamc.exe`, `yamcvm.exe`, `spvm.exe`, `spkg.exe`, `spid3r-self.exe`, and `spid3r-control.exe` (all registered under `windows.appExecutionAlias` so Windows automatically exposes them on `PATH` inside `%LOCALAPPDATA%\Microsoft\WindowsApps` without modifying registry `PATH`)
- `LLVM-C.dll`, `LLVM-C.lib`, SHA-256-pinned `spid3r_lang.lib`, `spid3r_lang.lib.sha256`, `runtime-manifest.json`
- `editor\spid3r-formatter.vsix`
- `.matrix` and `.mt` `windows.fileTypeAssociation`

## Submitting to Microsoft Partner Center

1. Download `Spid3r-<version>-x64.msix` from the GitHub Release assets (built by `.github/workflows/release.yml` via `tools/release/release_selfhost.sh` and Windows SDK `makeappx.exe`).
2. Upload the unsigned `.msix` directly to **Partner Center → Submissions → Packages**. Microsoft Store signs Store packages with your Partner Center certificate upon certification.
3. Because this is a desktop compiler toolchain (`runFullTrust`), include a short note in the **Submission notes for certification** explaining that Spid3r is a programming language CLI, bytecode VM, and LLVM AOT compiler (`spid3r`, `yamc`, `spvm`, `spkg`) with a local desktop settings panel (`spid3r-control`).

## Local Sideload Testing (Optional)

Microsoft Store `.msix` packages are left unsigned in CI. To sideload locally on Windows before submitting:

```powershell
signtool sign /fd SHA256 /f MyDevCert.pfx /p <password> Spid3r-1.5.1-x64.msix
```
