# Embervale launcher

Checks for updates, downloads what changed, starts the game.

One self-contained executable, around 7 MB, with nothing to install alongside
it: no WebView2, no .NET, no Node. It draws its own interface, so it looks the
same on every machine rather than inheriting whatever browser engine the OS
happens to ship.

    cargo run                  # debug, with a console for println!
    cargo build --release      # target/release/embervale-launcher.exe
    cargo test

## How it decides what to do

1. Fetch `manifest.json` from `manifest_url`.
2. For each file: compare size, then SHA-256, against what is on disk.
3. Download whatever differs, into `<file>.part`.
4. Verify the hash of what arrived; only then move it into place.
5. Enable **PLAY**, which starts `launch` and closes the launcher.

Nothing is trusted because it arrived. A download interrupted at 90% leaves a
`.part` file and the previous version still installed, rather than a plausible
looking executable that crashes on start.

## Configuring it

By default it reads:

    https://github.com/Omarimai/embervale-launcher/releases/download/live/manifest.json

Game builds are published into *this* repository, not the client's: the client
is private, and release assets on a private repository cannot be fetched without
credentials.

`live` is a fixed tag that each client build overwrites, so that URL keeps
resolving to the newest build without this being rebuilt -- which matters,
because a launcher already on a player's disk is the one thing a release cannot
update. A fixed tag rather than `/releases/latest/download/`, because that
redirect points at whichever release is newest repository-wide, and the
launcher's own releases live here too and carry no manifest.

To point a build at a different channel, put a `launcher.toml` beside the
executable:

```toml
manifest_url = "https://embervale.example/updates/manifest.json"
install_dir  = "game"          # relative paths resolve against the launcher
```

`EMBERVALE_MANIFEST_URL` overrides the URL again, which is what the test loop
below uses. A `launcher.toml` that does not parse is a hard error rather than a
silent fall back to the default channel -- the player edited it for a reason.

## The manifest

```json
{
  "version": "0.4.1",
  "launch": "Embervale.exe",
  "files": [
    {
      "path": "Embervale.exe",
      "sha256": "3b1f…",
      "size": 48210432,
      "url": "https://cdn.embervale.example/0.4.1/Embervale.exe"
    }
  ],
  "news": [
    { "title": "Inventory and equipment", "date": "2026-08-17",
      "body": "Characteristics, the bag, and loot that drops from packs." }
  ]
}
```

`news` is optional; without it the launcher shows only the art. Generate the
whole thing from an export directory:

    python tools/make_manifest.py \
        --dir dist/0.4.1 \
        --base-url https://cdn.embervale.example/0.4.1/ \
        --version 0.4.1 \
        --launch Embervale.exe \
        --out dist/0.4.1/manifest.json

## Trying it without a server

    cd /path/to/some/dist
    python -m http.server 8000
    EMBERVALE_MANIFEST_URL=http://127.0.0.1:8000/manifest.json cargo run

## Two things that will bite

**The launcher cannot replace itself while running.** Files install into
`game/`, a subdirectory, so the manifest can never name the launcher's own
executable -- `..` and absolute paths are refused outright. Self-update needs a
helper that swaps the exe after the launcher exits; that is not built yet.

**Unsigned binaries get flagged.** SmartScreen and Defender will warn on a
freshly published launcher until it earns reputation. A code signing
certificate is the only real fix.

## Releasing

```bash
git tag v0.1.0
git push origin v0.1.0
```

`.github/workflows/release.yml` runs the tests, builds, and attaches the binary
to the GitHub release as **EmbervaleLauncher.exe**. That name is fixed, because
the website links to `/releases/latest/download/EmbervaleLauncher.exe` and
should never need editing for a release.

The game is released separately, by tagging the client repo -- but it publishes
its build *here*, onto the `live` release. The two are independent: tagging the
client publishes a new manifest that every installed launcher picks up on its
next run, without anyone downloading a new launcher.

The two kinds of release share this repository without colliding, because only
one release can be the latest and the launcher's own needs to be it. Version
tags (`v0.1.0`, ...) are launcher builds and take `latest` in turn; `live` is
the game, is overwritten in place, and is pinned to `--latest=false`. The
client's workflow re-asserts that on every publish. If `live` ever became the
latest release, the website's download link would 404.

Publishing across repositories needs credentials the default workflow token does
not have, so the client repo holds a `LAUNCHER_RELEASE_TOKEN` secret: a
fine-grained PAT with contents:write on this repository.
