# Windows installer

The Windows installer is an [Inno Setup 6](https://jrsoftware.org/isinfo.php) package. It installs
Equalizerer and the VB-CABLE virtual audio driver that Equalizerer needs on Windows. The source is in
`installer/`; the build script is `scripts/build-installer.ps1`.

## Build

```powershell
winget install JRSoftware.InnoSetup   # once: Inno Setup 6 compiler
./scripts/build-installer.ps1         # produces installer/output/Equalizerer-Setup-<version>.exe
```

Options:

- `-SkipDownload`: fail instead of downloading the VB-CABLE driver pack when it has not been
  extracted yet (for offline or reproducible builds that already have `installer/vendor/vbcable/`).

The script:

1. Reads the version from `[workspace.package] version` in the root `Cargo.toml`.
2. Runs `cargo build --release -p eq-app`.
3. Makes sure `installer/vendor/vbcable/VBCABLE_Setup_x64.exe` exists. If not, it downloads the
   official pack `https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip` to
   `installer/vendor/VBCABLE_Driver_Pack45.zip`, prints its SHA-256, and extracts it to
   `installer/vendor/vbcable/`. It also prints the Authenticode signature status of the setup.
4. Finds `ISCC.exe` (per-user install, `Program Files (x86)`, `Program Files`, or `PATH`) and compiles
   `installer/equalizerer.iss` with `/DAppVersion=<version>` and `/DRepoRoot=<repo root>`.

Any failure stops the script with a non-zero exit code. Both `installer/vendor/` and
`installer/output/` are git-ignored.

A release build fails if a running `eq-app.exe` locks the output file; close Equalizerer first.

## What the setup does

Setup requires administrator rights (the driver install needs them).

1. **Close check:** if Equalizerer is running (its `equalizerer` mutex), setup asks the user to close it.
2. **VB-CABLE check:** before any file is copied, setup looks for a present audio endpoint named
   `*VB-Audio Virtual Cable*` (the PnP name of the render endpoint is "Speakers (VB-Audio Virtual Cable)", not "CABLE Input") with PowerShell (`Get-PnpDevice -PresentOnly -Class AudioEndpoint`). The result is
   stored once and used by every later step.
3. **Files:** `eq-app.exe` goes to `C:\Program Files\Equalizerer\`. If VB-CABLE is missing, its setup
   files are copied to `{app}\vbcable\` (kept for uninstall) and to a temporary folder (for install).
4. **Shortcuts:** a Start Menu entry, and an optional desktop shortcut (unchecked by default).
5. **Autostart (task "Start Equalizerer with Windows", checked by default):** writes
   `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Equalizerer` =
   `"<install dir>\eq-app.exe" --minimized`, the same value the app's own checkbox writes.
6. **VB-CABLE install:** if the check found no device, runs `VBCABLE_Setup_x64.exe -i -h` silently.
   Windows may show its driver prompt once (see Limitations).
7. **Registry flag:** if this setup installed VB-CABLE, writes `HKLM\Software\Equalizerer`,
   `InstalledVBCable = 1`.
8. **Wait for the device:** polls up to about 10 seconds for the VB-CABLE endpoint. If it does
   not appear, setup shows a message that a restart may be needed.
9. **Launch:** optional "Launch Equalizerer" on the last page, run as the logged-on user.

## Uninstall

1. Setup asks the user to close Equalizerer if it is running.
2. If `InstalledVBCable = 1` (VB-CABLE was installed by this setup, not by the user earlier), a
   question appears with default **No**: "Also remove the VB-CABLE virtual audio device? Keep it if
   other apps use it." The question is skipped in silent uninstalls, so VB-CABLE is kept there.
3. Before any file is removed, the uninstaller runs `eq-app.exe --restore`, which makes the real
   output device the default again.
4. If the user answered Yes, it runs `{app}\vbcable\VBCABLE_Setup_x64.exe -u -h`.
5. It removes the program files, the Start Menu and desktop shortcuts, the autostart value and the
   `HKLM\Software\Equalizerer` key, and deletes `{app}\vbcable`.

## Licensing

Equalizerer is MIT licensed (`installer/LICENSE.txt`).

VB-CABLE is donationware by VB-Audio Software (https://vb-cable.com). The installer redistributes its
official setup, including silent installation, under the donationware model. The installer shows the
attribution and the request to support the author on its information page
(`installer/THIRD_PARTY.txt`). Keep that attribution when changing the installer.

## Known limitations

- **No code signing.** Windows SmartScreen warns about the unsigned setup and the unsigned
  `eq-app.exe`. Signing is a backlog item (see [PLAN.md](PLAN.md)).
- **Windows driver prompt.** Windows asks once to approve "Install this device software?" for the
  VB-CABLE driver. The installer does not suppress this prompt and does not change certificate or
  driver-signing settings.
- **Admin-mode autostart.** Because the installer requires administrator rights, the `HKCU` autostart
  value goes to the hive of the account that elevated the installer. If that account differs from the
  user's own account, autostart is not enabled for the user.
- **Endpoint check depends on the PnP class.** The VB-CABLE detection queries the `AudioEndpoint`
  PnP class. This is untested on real hardware; if it does not match, setup would reinstall VB-CABLE
  on each run.
- **Failed VB-CABLE install.** The registry flag is written before the driver install is confirmed, so
  an interrupted VB-CABLE install still offers removal later. Removing a missing device is harmless.
- **Manual test pending.** Install, upgrade, uninstall, and the VB-CABLE uninstall question have not
  been tested on a real Windows machine yet (see T-7.3 in [PLAN.md](PLAN.md)).
