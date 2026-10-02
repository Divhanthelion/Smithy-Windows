# Publishing Smithy to the Microsoft Store

Smithy ships as an MSIX package: a full-trust desktop app (`runFullTrust`),
one statically linked `smithy.exe` with no runtime to install, and the tile
icons in `Assets\`. The Store signs the package; nothing here needs a
certificate.

## Once: Partner Center

1. **Developer account.** Sign up at https://partner.microsoft.com/dashboard
   as an individual (check the current fee there).
2. **Reserve the name** (Apps and games → New product → MSIX or PWA app).
   "Smithy" may be taken; have a second choice ready ("Smithy Code",
   "Smithy Editor").
3. **Copy the identity** from Product management → Product identity:
   `Package/Identity/Name`, `Package/Identity/Publisher` and
   `Package/Properties/PublisherDisplayName`.

## Each release

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows-store\build-msix.ps1 `
    -IdentityName "<Package/Identity/Name>" `
    -Publisher "<Package/Identity/Publisher>" `
    -PublisherDisplayName "<PublisherDisplayName>"
```

This builds the release binary, stages `target\msix\layout\`, and writes
`target\msix\Smithy_<version>_x64.msix`. Raise `version` in the workspace
`Cargo.toml` for each submission (the Store refuses a version it has seen).

Before uploading, install the same build locally and click through it (needs
Developer Mode, under Settings → System → For developers):

```powershell
Add-AppxPackage -Register target\msix\layout\AppxManifest.xml
```

Then in Partner Center: Start submission → Packages (upload the `.msix`) →
Store listings (text in `STORE-LISTING.md`) → Properties → Age ratings → Submit.

## What the submission will ask

| Item | Answer |
|---|---|
| Privacy policy URL | https://github.com/Divhanthelion/Smithy-Windows/blob/main/PRIVACY.md |
| Website / support | https://github.com/Divhanthelion/Smithy-Windows (issues for support) |
| Category | Developer tools |
| Why `runFullTrust` | A code editor: it opens and edits the user's project folders anywhere on disk, runs their builds and tests through Git Bash, and spawns a language server (rust-analyzer) when installed. |
| Microphone | Optional dictation into the agent's prompt, recognised on the device. |
| Age rating | No user-to-user content, no purchases. The agent's replies come from a model the user chooses with their own key. |
| Store screenshots | At least one, 1366×768 or larger, of the Windows app (not the macOS screenshot in `assets/`). |
| Logo | `listing-logo-1080.png` (made by `make-icons.ps1`). |

## Known gaps before a public launch

- **The icon is a placeholder** (a gold serif S drawn by `make-icons.ps1`).
  Replace `Assets\*.png` at the same sizes.
- **Git for Windows is not bundled.** Without it the agent cannot run
  commands; Smithy says so on connect with the install link, and the listing
  says so too. Bundling MinGit (about 50 MB) is the alternative.
- **rust-analyzer is not bundled**: code intelligence for Rust projects needs
  it installed (`rustup component add rust-analyzer`).
- **Store policy on generative AI.** Read the current Microsoft Store Policies
  for apps that generate content with AI before submitting; the content comes
  from third-party models the user chooses, which the listing should say.
- **x64 only.** An ARM64 package needs an `aarch64-pc-windows-msvc` build
  and a second architecture in the bundle.
