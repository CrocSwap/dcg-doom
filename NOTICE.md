# Notice

- **This repository** (the DCG Doom program, relayer, page and tooling) is
  licensed under the GNU General Public License version 3 (`LICENSE`), the
  same terms as the durable-compute-graph crates it links.
- **The Doom engine** is id Software's `linuxdoom-1.10`, fetched at build time
  by `stunts/doom-host/build.sh` from github.com/id-Software/DOOM at a pinned
  commit and never committed here. The patches under
  `stunts/doom-host/patches/` and the adapted engine code under
  `stunts/doom-sbf/` are derived from it and carry its license
  (see `stunts/doom-sbf/LICENSE-NOTE.md` and `stunts/doom-host/LICENSE-NOTE.md`).
- **The game data** (`doom1.wad`, the shareware IWAD) is downloaded by
  `stunts/doom-host/build.sh` and checked against its sha256. It is never
  committed. Two small test fixtures derived from a game session are
  committed: `stunts/doom-dcg/tests/fixtures/commands.bin` (input commands)
  and `web/doom-play/test/fixtures/frame-cursor-6016-strips.bin.gz` (one
  rendered frame).

History: this repository was split out of the Basanos repository on
2026-10-09 with `git filter-repo`, keeping the history of the moved paths.
The earlier R-series Doom program stays in Basanos's history only.
