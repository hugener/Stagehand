# Stagehand

Small desktop tools that stay out of the way. Stagehand runs in the background, lives in
the system tray, and hosts a set of tools you can switch on and off.

It is at an early stage: the tools are being ported in one at a time.

## Tools

- **Laser Pointer** – draw a fading trail on screen with the mouse or a pen while
  presenting. Press Win+Shift+L to turn it on, then hold the middle mouse button to draw.
- **Presenter Window** – mirrors a window into one you can share in a call, with the
  laser trail on it, so viewers see the trail without you sharing the whole screen. Point
  at a window and press Win+Ctrl+Shift+W, then share "Presenter Window" in the call.
  Win+Ctrl+Shift+Q stops it.
- **Taskbar Switch** – moves the Windows taskbar to a different edge when the keyboard is
  detached or folded away. Off until you switch it on.

## Building

Stagehand is written in Rust with a [Slint](https://slint.dev) user interface, and
currently supports Windows only.

```
cargo run -p stagehand
```

Starting Stagehand opens its settings. Starting it again while it is running brings the
settings back to the front. `--background` starts it in the tray without opening them.
 are kept in `%APPDATA%\Stagehand\settings.json` and the log in
`%LOCALAPPDATA%\Stagehand\logs`.

## Packaging for the Microsoft Store

```
packaging\build-msix.ps1
```

builds `target\msix\Stagehand_<version>_<architecture>.msix` from the manifest and logos
in `packaging`. It needs the Windows SDK for `makeappx.exe`.

- **For a Store release**, pass the identity Partner Center shows under Product identity:
  `-IdentityName`, `-Publisher` and `-PublisherDisplayName`. The package is not signed;
  the Store signs what is submitted to it.
- **To try the packaged app on your own machine**, add `-Register`. Windows will not
  install an unsigned package, so this registers the layout folder directly, which
  needs Developer Mode. `Get-AppxPackage -Name Stagehand.Development | Remove-AppxPackage`
  removes it again.

Packaged, Stagehand starts at sign-in through the package's startup task instead of the
`Run` registry key, and Windows keeps its settings and log in the package's own folder
under `%LOCALAPPDATA%\Packages`. The first time a window is shared, Windows asks whether
Stagehand may capture without its yellow outline.

## Releasing

1. Set the new version in `Cargo.toml` and get that onto `main`.
2. Publish a release on GitHub with the tag `v<version>`, such as `v0.2.0`.

The `Release` workflow then builds both architectures and attaches to the release, for
each one, the Store package and the program on its own. It stops if the tag and the
version in `Cargo.toml` differ. Running the workflow by hand from the Actions tab does
the same build without attaching anything, to check beforehand that a release will work.

The packages carry the Store identity, which is set at the top of
`.github\workflows\release.yml`. Submitting them to the Store is done by hand in Partner
Center: upload both `.msix` files to one submission.

## Icons

The logo is drawn in `assets\logo.svg`, with a bolder `assets\logo-small.svg` for
sizes of 32 pixels and under. After changing either, run

```
assets\make-icons.ps1
```

which renders them with Microsoft Edge into the package logos in `packaging\Assets`
and the program, notification area and window icons in `crates\stagehand\assets`.
Those files are checked in, so a build does not need to run it.

## Layout

| Crate | Contents |
|---|---|
| `crates/stagehand-core` | Plugin contract, plugin host and settings storage; no OS or UI dependencies |
| `crates/stagehand-platform` | Operating system integration: single instance, start at sign-in, paths |
| `crates/presenter-window` | The Presenter Window plugin: window capture, the mirror and its outline |
| `crates/taskbar-switch` | The Taskbar Switch plugin |
| `crates/laser-pointer` | The Laser Pointer plugin: trail model, overlay, mouse and pen input |
| `crates/stagehand` | The application: tray icon, settings window and log |
| `packaging` | The Microsoft Store package: manifest, logos and build script |
| `assets` | The logo drawings and the script that makes the icon files from them |

Tools are plugins compiled into the application and registered in
`crates/stagehand/src/plugins.rs`.

## Licence

Stagehand is released under the [MIT licence](LICENSE).

The user interface is built with Slint, used under the Slint Royalty-free Desktop,
Mobile, and Web Applications License.
