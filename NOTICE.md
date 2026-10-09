# Notice

- **This repository is licensed under the GNU General Public License
  version 3** (`LICENSE`), the same terms as the durable-compute-graph crates
  it links. The SBF program also links the Solana SDK crates (Apache-2.0,
  compatible with GPL v3).
- **The Doom engine** is id Software's `linuxdoom-1.10`, fetched at build time
  by `stunts/doom-host/build.sh` from github.com/id-Software/DOOM at a pinned
  commit and never committed here. id's 1999 release ships the GNU GPL v2
  text; its source headers name no GPL version and grant no "or later"
  option. We use it, our patches to it (`stunts/doom-host/patches/`) and the
  engine code adapted in `stunts/doom-sbf/` under GPL v3, relying on GPL v2
  section 9 (a program that specifies no version may be used under any
  version). That is our good-faith reading for a non-commercial
  demonstration, not a statement by id Software.
- **The game data** (`doom1.wad`, the shareware IWAD) is downloaded by
  `stunts/doom-host/build.sh` and checked against its sha256. It is never
  committed. Two small test fixtures derived from a game session are
  committed: `stunts/doom-dcg/tests/fixtures/commands.bin` (input commands)
  and `web/doom-play/test/fixtures/frame-cursor-6016-strips.bin.gz` (one
  rendered frame).

History: this repository was split out of the Basanos repository on
2026-10-09 with `git filter-repo`, keeping the history of the moved paths.
The earlier R-series Doom program stays in Basanos's history only.
