# Releasing nu_plugin_render

Pushing a version tag such as `v0.2.0` starts the release.
`.github/workflows/release.yml` builds `render` for six targets with
[dist](https://github.com/axodotdev/cargo-dist) and publishes the GitHub
release. crates.io comes last, by hand.

Run these from the repository's root. Set the version once, and the
commands below use it:

```nu
let v = "0.2.0"
```

## 1. Set the version

- Change `version` in `Cargo.toml` to the new version.
- If `nu-plugin` changed, change the Nushell version in the README's
  Install section too.
- Build, so `Cargo.lock` picks up the new version, then commit both files
  and push `main`:

```nu
cargo build
git add Cargo.toml Cargo.lock
git commit -m $"Release ($v)"
git push origin main
```

## 2. Tag and push the tag

The tag is the version with a `v` in front. Pushing it starts the workflow.

```nu
git tag -m Release $"v($v)" main
git push origin $"v($v)"
```

If the tag doesn't match the version in `Cargo.toml`, dist stops with an
error and publishes nothing.

## 3. Watch the workflow

```nu
gh run watch --repo fdncred/nu_plugin_render
```

Each target builds on its own machine:

| Target                      | Machine                     |
| --------------------------- | --------------------------- |
| `aarch64-apple-darwin`      | `macos-latest`              |
| `x86_64-apple-darwin`       | `macos-latest`, cross-built |
| `aarch64-unknown-linux-gnu` | `ubuntu-22.04-arm`          |
| `x86_64-unknown-linux-gnu`  | `ubuntu-22.04`              |
| `aarch64-pc-windows-msvc`   | `windows-11-vs2026-arm`     |
| `x86_64-pc-windows-msvc`    | `windows-2022`              |

## 4. Check the release page

`https://github.com/fdncred/nu_plugin_render/releases/tag/v<version>` should
have:

- `nu_plugin_render-aarch64-apple-darwin.tar.xz`
- `nu_plugin_render-x86_64-apple-darwin.tar.xz`
- `nu_plugin_render-aarch64-unknown-linux-gnu.tar.xz`
- `nu_plugin_render-x86_64-unknown-linux-gnu.tar.xz`
- `nu_plugin_render-aarch64-pc-windows-msvc.zip`
- `nu_plugin_render-x86_64-pc-windows-msvc.zip`
- `nu_plugin_render-installer.sh` and `nu_plugin_render-installer.ps1`
- a `.sha256` file for each archive, `sha256.sum`, `source.tar.gz` and
  `dist-manifest.json`

## 5. Test the download

Install into a scratch folder, so the `nu_plugin_render` in `~/.cargo/bin`
stays as it is. `--disable-strategies compile` makes binstall fail when it
can't find the download, instead of quietly building from source.

```nu
cargo binstall --git https://github.com/fdncred/nu_plugin_render nu_plugin_render --install-path /tmp/render-release --no-track --disable-strategies compile
```

Then run it with a registry of its own, so your registered plugins stay
as they are:

```nu
nu --plugin-config /tmp/render-release/registry.msgpackz --plugins '[/tmp/render-release/nu_plugin_render]'
```

Inside that `nu`, `ls | render` should draw and `version` in `plugin list`
should be the new version.

## 6. Publish to crates.io

Last, so every version on crates.io has its binaries. A published version
can't be changed, only yanked.

```nu
cargo publish
```

After the first release, the repo's website can point at crates.io:

```nu
gh repo edit fdncred/nu_plugin_render --homepage https://crates.io/crates/nu_plugin_render
```

## If a build fails

dist publishes nothing unless every build succeeds, so the tag can be
used again. Delete it here and on GitHub, fix the problem, commit, push
`main`, and go back to step 2.

```nu
git tag -d $"v($v)"
git push origin --delete $"v($v)"
```

## How the pieces fit

- dist writes `.github/workflows/release.yml` from `dist-workspace.toml`.
  Don't edit the workflow. Change `dist-workspace.toml` and run
  `dist generate` with the dist version its `cargo-dist-version` pins.
  `dist generate --check` says whether the workflow is out of date.
- Both macOS builds run on `macos-latest`, set in
  `[dist.github-custom-runners]`. dist's own defaults are pinned images
  that GitHub retires.
- Pull requests run only `dist plan`. Nothing builds or publishes until a
  tag is pushed.
- `[profile.dist]` in `Cargo.toml` is the profile release builds use: the
  release profile with thin LTO and symbols stripped.
- `cargo binstall nu_plugin_render` reads `[package.metadata.binstall]` from
  the crate on crates.io and downloads the archive for the user's platform
  from the GitHub release. The archive names and layout there must match
  what dist builds: the binary sits in a folder named after the archive,
  except in the Windows zips, which have no folder.
