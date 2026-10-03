# MetalHLE 2.0

**MetalHLE 2.0** is an independent, community-maintained fork of [touchHLE](https://github.com/touchHLE/touchHLE), carrying forward work from HyperHLE contributors. It is a high-level emulator for early iPhone OS apps: selected iOS frameworks are reimplemented on the host so compatible apps can run without booting iOS. This fork focuses on Android/mobile usability, fullscreen and orientation handling, graphics compatibility, and app-specific fixes. It is not an official Apple product.

The repository does not distribute commercial apps or iOS firmware. Put an app bundle (`.app`) or IPA (`.ipa`) that you are entitled to use in `touchHLE_apps/`; it will appear in the app picker.

## Platforms and features

- Builds for Android, Windows, macOS, and Linux.
- Virtual iPhone and iPad device profiles.
- Android touch input and fullscreen/orientation handling.
- Ongoing GLES, EAGL, Core Animation, audio, and iOS framework compatibility work.
- Download build artifacts from the [MetalHLE 2.0 GitHub Actions workflow](https://github.com/RadekParek/MetalHLE/actions/workflows/HyperHLE_release.yml).

## Newly tested and working games

The titles below have been tested with MetalHLE 2.0 and are known to run. Compatibility may still vary with the app version, virtual device profile, host GPU/driver, and settings; this list does not guarantee that every feature or a full playthrough works.

- N.O.V.A. 3 (Playable)
- Gangstar Vegas (Unplayable)
- Geometry Dash (2.11 and 1.0 + playable)
- Terraria (1.0 + playable)
- Modern Combat 3 (1.5.0 + playable)
- Asphalt 7 (Playable)
- Turbo Dismount (Pretty playable)
- Scarface (Playable)
- Zombie Safari (Playable)
- Real Racing 1 (Playable)
- Silent Ops (Playable)
- Need For Speed: Most Wanted (Unplayable)
- Minecraft 0.14.2-0.16.2 (Playable)
- Oceanhorn (Unplayable)
- Asphalt 8 (1.0.0 + maybe playable)
- And more

## Build and documentation

Build and run a desktop version with Cargo:

```sh
cargo run --release -- path/to/app.app
```

For Android build steps, see the `android/` directory and the [CI workflow](https://github.com/RadekParek/MetalHLE/actions/workflows/HyperHLE_release.yml). Command-line options are documented in [`OPTIONS_HELP.txt`](OPTIONS_HELP.txt); notable compatibility changes are tracked in [`CHANGELOG.md`](CHANGELOG.md).

## Community

Join the [MetalHLE community Discord](https://discord.gg/ZpEkAV47H9) to discuss the project and contribute.

## Credits and license

MetalHLE 2.0 carries forward work from touchHLE and HyperHLE contributors and uses open-source libraries. It is licensed under the [Mozilla Public License 2.0](LICENSE); see the individual projects for their respective notices and licenses.
