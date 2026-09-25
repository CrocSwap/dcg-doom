"""Offline checks for the Doom play site and the Doom relayer.

The JavaScript suites are run from Python so `pytest` is still the single
command. Nothing here touches the network or a key file.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
PAGE = ROOT / "web" / "doom-play"
RELAYER = ROOT / "stunts" / "doom-relayer"

VIEWER_SUITES = [
    PAGE / "test" / "test_strip.mjs",
    PAGE / "test" / "test_input.mjs",
    PAGE / "test" / "test_palette.mjs",
]
RELAYER_SUITES = [RELAYER / "test" / "test_relayer.mjs"]

PAGE_MODULES = ["app.js", "burner.js", "input.js", "network.js", "rpc.js", "strip.js"]
RELAYER_MODULES = ["pool.mjs", "sender.mjs", "server.mjs"]


def run_node(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["node", *args],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=300,
        check=False,
    )


@pytest.mark.skipif(shutil_which_node := __import__("shutil").which("node") is None, reason="node is not installed")
class TestDoomPlay:
    def test_every_module_parses_as_an_es_module(self):
        # --check on a .js file with no package.json would parse as CommonJS
        # and miss a real syntax error, so the page carries "type": "module"
        # and this asserts that it is really there.
        assert json.loads((PAGE / "package.json").read_text())["type"] == "module"
        for name in PAGE_MODULES:
            result = run_node("--check", str(PAGE / name))
            assert result.returncode == 0, f"{name}: {result.stderr}"

    @pytest.mark.parametrize("name", RELAYER_MODULES)
    def test_relayer_modules_parse(self, name):
        result = run_node("--check", str(RELAYER / name))
        assert result.returncode == 0, f"{name}: {result.stderr}"

    def test_viewer_suites_pass(self):
        result = run_node("--test", *[str(path) for path in VIEWER_SUITES])
        assert result.returncode == 0, result.stdout[-6000:] + result.stderr[-2000:]

    def test_relayer_suite_passes(self):
        result = run_node("--test", *[str(path) for path in RELAYER_SUITES])
        assert result.returncode == 0, result.stdout[-6000:] + result.stderr[-2000:]

    def test_the_page_never_sends_a_transaction(self):
        # The page reads and signs; it must not contain a send path. The
        # relayer is the only thing that sends.
        for name in PAGE_MODULES:
            source = (PAGE / name).read_text()
            for forbidden in ("sendTransaction", "signAndSend", "sendRawTransaction", "requestAirdrop"):
                assert forbidden not in source, f"{name} references {forbidden}"
        # A read-only RPC client must not call a write method.
        rpc = (PAGE / "rpc.js").read_text()
        for method in ("sendTransaction", "simulateTransaction"):
            assert f'"{method}"' not in rpc, f"rpc.js calls {method}"

    def test_no_wallet_extension_code(self):
        for name in PAGE_MODULES:
            source = (PAGE / name).read_text().lower()
            for forbidden in ("solana/web3.js", "window.solana", "window.phantom", "walletadapter", "solders"):
                assert forbidden not in source, f"{name} references {forbidden}"

    def test_no_absolute_machine_paths(self):
        for path in [*PAGE.rglob("*.js"), *PAGE.rglob("*.mjs"), *PAGE.rglob("*.md"), RELAYER / "README.md"]:
            if path.name == "test_strip_live.mjs":
                # That one takes the receipts path as an argument on purpose.
                continue
            assert "/Users/" not in path.read_text(), f"{path} hard-codes a home directory"

    def test_network_values_live_in_one_file(self):
        # The brief: one network config file, nothing network-specific
        # elsewhere. Testnet URLs and the program id must appear only in
        # network.js, and the same holds for the relayer.
        program_id = "57kJfNC3U4gZ6VBDpbYRmwA14WYDXhiS75aaj6PwA9kT"
        rpc_host = "testnet.fogo.io"
        for path in [*PAGE.rglob("*.js"), *PAGE.rglob("*.mjs"), *PAGE.rglob("*.html"), *RELAYER.rglob("*.mjs")]:
            if path.name == "network.js":
                continue
            source = path.read_text()
            assert program_id not in source, f"{path} repeats the Doom program id"
            assert rpc_host not in source, f"{path} repeats the testnet RPC host"

    def test_mainnet_is_a_placeholder_and_is_refused(self):
        result = run_node(
            "--input-type=module",
            "-e",
            "const {NETWORKS, requireNetwork, isMainnet} = await import('./web/doom-play/network.js');"
            "const out = {isMainnet: isMainnet(NETWORKS.mainnet)};"
            "try { requireNetwork('mainnet', ['rpcHttp']); out.threw = false; }"
            "catch (e) { out.threw = true; out.message = e.message; }"
            "try { requireNetwork('mainnet', ['doom.program']); out.doomThrew = false; }"
            "catch (e) { out.doomThrew = true; out.doomMessage = e.message; }"
            "try { requireNetwork('mainnet', []); out.noFieldsThrew = false; }"
            "catch (e) { out.noFieldsThrew = true; }"
            "console.log(JSON.stringify(out));",
        )
        assert result.returncode == 0, result.stderr
        report = json.loads(result.stdout.strip().splitlines()[-1])
        assert report["isMainnet"] is True
        assert report["threw"] is True, "an unfilled mainnet rpcHttp must be refused by name"
        assert "rpcHttp" in report["message"]
        assert "network.js" in report["message"], "the error says where to fix it"
        assert report["doomThrew"] is True, "an unfilled mainnet has no Doom program to point at"
        assert "doom.program" in report["doomMessage"]
        # Asking for no fields checks nothing, which is the intended contract:
        # the caller names what it is about to use.
        assert report["noFieldsThrew"] is False

    def test_the_reader_refuses_mainnet_without_an_explicit_opt_in(self):
        result = run_node(
            "--input-type=module",
            "-e",
            "const {ChainReader} = await import('./web/doom-play/rpc.js');"
            "let out = {};"
            "try { new ChainReader({network: 'mainnet'}); out.refused = false; }"
            "catch (e) { out.refused = true; out.message = e.message; }"
            "console.log(JSON.stringify(out));",
        )
        assert result.returncode == 0, result.stderr
        report = json.loads(result.stdout.strip().splitlines()[-1])
        assert report["refused"] is True
        assert "mainnet" in report["message"]

    def test_testnet_is_fully_configured(self):
        result = run_node(
            "--input-type=module",
            "-e",
            "const {requireNetwork} = await import('./web/doom-play/network.js');"
            "const n = requireNetwork('testnet', ['rpcHttp','doom.program','doom.context','doom.strips',"
            "'doom.snapshots','doom.wadAccounts','explorer.address','explorer.transaction']);"
            "console.log(JSON.stringify({strips: n.doom.strips.length, snaps: n.doom.snapshots.length,"
            " wads: n.doom.wadAccounts.length, paint: n.commitment.paint, reconcile: n.commitment.reconcile}));",
        )
        assert result.returncode == 0, result.stderr
        report = json.loads(result.stdout.strip().splitlines()[-1])
        assert report["strips"] == 2, "two render buffers"
        assert report["snaps"] == 2
        assert report["wads"] == 5
        assert report["paint"] == "processed"
        assert report["reconcile"] == "confirmed"

    def test_strip_addresses_are_all_base58_and_distinct(self):
        result = run_node(
            "--input-type=module",
            "-e",
            "const {NETWORKS} = await import('./web/doom-play/network.js');"
            "const d = NETWORKS.testnet.doom;"
            "const all = [...d.strips.flat(), ...d.snapshots, ...d.wadAccounts, d.context, d.meta,"
            " d.inputBuffer, d.authority, d.program];"
            "console.log(JSON.stringify({count: all.length, unique: new Set(all).size,"
            " bad: all.filter((a) => !/^[1-9A-HJ-NP-Za-km-z]{32,44}$/.test(a))}));",
        )
        assert result.returncode == 0, result.stderr
        report = json.loads(result.stdout.strip().splitlines()[-1])
        assert report["bad"] == [], f"not base58: {report['bad']}"
        assert report["unique"] == report["count"], "an address is repeated"

    def test_the_burner_key_note_is_visible_on_the_page(self):
        page = (PAGE / "index.html").read_text()
        assert "THROWAWAY TEST KEY" in page
        assert "no wallet extension" in page
        assert 'id="burner-key"' in page
        source = (PAGE / "burner.js").read_text()
        assert "throwaway" in source.lower()
        assert "no funds" in source.lower() or "holds no funds" in source.lower()

    def test_both_themes_are_defined_and_the_toggle_cannot_flash(self):
        css = (PAGE / "style.css").read_text()
        assert ":root[data-theme=\"light\"]" in css
        assert "prefers-color-scheme" in (PAGE / "index.html").read_text() or 'color-scheme: dark light' in (
            PAGE / "index.html"
        ).read_text()
        # The theme is set by a blocking inline script before the stylesheet,
        # so a stored light theme does not flash dark.
        html = (PAGE / "index.html").read_text()
        assert html.index("doom-play.theme") < html.index('<link rel="stylesheet"')

    def test_the_page_is_phone_shaped(self):
        css = (PAGE / "style.css").read_text()
        assert "width=device-width, initial-scale=1" in (PAGE / "index.html").read_text()
        assert "max-width: 720px" in css
        assert "@media (prefers-reduced-motion: reduce)" in css
        assert ".pad" in css, "there are touch controls"

    def test_the_relayer_refuses_to_start_in_chain_mode(self):
        result = run_node(
            "--input-type=module",
            "-e",
            "const {createSender} = await import('./stunts/doom-relayer/sender.mjs');"
            "let out = {};"
            "try { createSender({mode: 'chain'}); out.threw = false; }"
            "catch (e) { out.threw = true; out.code = e.code; out.message = e.message; }"
            "console.log(JSON.stringify(out));",
        )
        assert result.returncode == 0, result.stderr
        report = json.loads(result.stdout.strip().splitlines()[-1])
        assert report["threw"] is True
        assert report["code"] == "not_wired"
        assert "doom_r8b_tokyo_pipeline" in report["message"]

    def test_the_relayer_does_not_own_a_key(self):
        # The relayer pays fees, so in chain mode it will need a key, but this
        # dispatch must not ship one or read one.
        for path in RELAYER.rglob("*"):
            if path.is_file():
                assert path.suffix in {".mjs", ".md"}, f"unexpected file in the relayer: {path}"
        readme = (RELAYER / "README.md").read_text()
        assert "no keys" in readme.lower()

    def test_the_design_note_records_the_slot_pool_problem(self):
        note = (ROOT / "docs" / "design" / "doom-relayer-api-2026-09-25.md").read_text()
        assert "6,016" in note
        assert "extend-capacity" in note.lower() or "EXTEND-CAPACITY" in note
        assert "capacity-extension" in note, "the recommendation names the actual change"
        assert "71.70204" in note, "the measured rent is cited rather than estimated"
        assert "71,702" not in note and "measured" in note
