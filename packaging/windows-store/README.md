# Publishing Smithy to the Microsoft Store

Smithy ships as an MSIX package: a full-trust desktop app (`runFullTrust`),
one statically linked `smithy.exe` with no runtime to install, and the tile
icons in `Assets\`. The Store signs the package; nothing here needs a
certificate.

## Once: Partner Center

1. **Developer account.** Sign up at https://partner.microsoft.com/dashboard
   as an individual. Since September 2025 that is free, with an ID check
   instead of a card.
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

Then run the Windows App Certification Kit on the package, the same checks
the Store runs first. It needs an administrator PowerShell:

```powershell
& "${env:ProgramFiles(x86)}\Windows Kits\10\App Certification Kit\appcert.exe" test `
    -appxpackagepath "$PWD\target\msix\Smithy_0.1.0.0_x64.msix" `
    -reportoutputpath "$PWD\target\msix\wack-report.xml"
```

Then in Partner Center: Start submission → Packages (upload the `.msix`) →
Store listings (text in `STORE-LISTING.md`) → Properties → Age ratings →
Submission options (the certification notes in `STORE-LISTING.md`, with a
test key) → Submit.

## Store Policies this has to meet

Checked against Microsoft Store Policies 7.20 (14 September 2026).

| Policy | What it asks | How Smithy meets it |
|---|---|---|
| 10.2.4 | A dependency on other software or a service is disclosed at the start of the description | The description opens with the API key and Git for Windows |
| 10.3.1 | A tester can try what needs credentials | A credit-limited OpenRouter key in the certification notes, made for the review and deleted after |
| 10.5.1 | A privacy policy (always, for a full-trust app) | `PRIVACY.md`, by URL |
| 10.2.7 | A clean uninstall | MSIX removes the app; `PRIVACY.md` says where the settings, conversations and key live, to remove those too |
| 11.16 | Live generative AI: disclosed in the metadata and in Partner Center, and a way for users to report a reply to the developer, acted on | The description says so; a *report* button under every reply and Agent → *Report an AI Reply…* open the `ai-content` issue form. **You** answer those reports (change the shipped instructions, or pass it to the provider) |
| 10.1.1 | The value is clear on first run | First launch opens the setup dialog, which says what each backend needs |

## What the submission will ask

| Item | Answer |
|---|---|
| Privacy policy URL | https://github.com/Divhanthelion/Smithy-Windows/blob/main/PRIVACY.md |
| Website / support | https://github.com/Divhanthelion/Smithy-Windows (issues for support) |
| Category | Developer tools |
| Why `runFullTrust` | A code editor: it opens and edits the user's project folders anywhere on disk, runs their builds and tests through Git Bash, and spawns a language server (rust-analyzer) when installed. |
| Microphone | Optional dictation into the agent's prompt, recognised on the device. |
| Generative AI | Yes, the app shows live generative AI content (Partner Center asks; policy 11.16). |
| Age rating (IARC questionnaire) | Category: utility/productivity. No violence, sex, language, drugs or gambling content of its own; no user-to-user communication or sharing; no location sharing; no digital purchases. It does reach the internet: the agent fetches web pages and searches, so answer yes to unrestricted internet access. The replies come from a third-party model the user chooses with their own key. |
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
- **The report form is public.** It is a GitHub issue form, so reporting
  needs a GitHub account and the report is visible to anyone (the form says
  so). A private address for reports, an email or a form, would be kinder;
  change `REPORT_URL` in `crates/smithy-editor/src/link.rs` and the
  certification notes.
- **No screenshots yet.** The listing needs at least one of the Windows app,
  1366×768 or larger: a conversation with a diff in Review shows it best.
- **x64 only.** An ARM64 package needs an `aarch64-pc-windows-msvc` build
  and a second architecture in the bundle.
