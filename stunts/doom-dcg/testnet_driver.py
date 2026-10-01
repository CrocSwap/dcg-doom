#!/usr/bin/env python3
"""Fixed-plan Doom-on-DCG rehearsal and testnet benchmark driver.

The application builds its own Doom instructions, input order, frame cursor
policy, and snapshot hashes. DCG's finite sequencer owns signed-packet
journaling, confirmation, and exact-byte retries. EndpointPool controls all
RPC pacing/routing; RpcSendProvider is the only transaction send path.

The `provision` and `campaign` commands are resumable. Invoke each under
scripts/stage_cap.sh; if a 90-second wrapper kills it, invoke the same command
again to resume its exact sequencer journal.
"""

from __future__ import annotations

import argparse
import asyncio
import base64
import hashlib
import json
import math
import os
import secrets
import stat
import subprocess
import sys
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable, Sequence

from solders.compute_budget import set_compute_unit_limit
from solders.hash import Hash
from solders.instruction import AccountMeta, Instruction
from solders.keypair import Keypair
from solders.message import Message
from solders.pubkey import Pubkey
from solders.signature import Signature
from solders.transaction import Transaction

from dcg.sequencer import (
    Backoff,
    Commitment,
    EndpointLimits,
    JournalStore,
    KeypairFileSigner,
    PostconditionResult,
    ProgramRefused,
    RetryPolicy,
    RpcConfig,
    Sequencer,
    SequencerConfig,
    SignedTransaction,
    SolanaRpcEndpoint,
    TransactionPlan,
    TransactionStep,
)
from dcg.sequencer.health import classify
from dcg.sequencer.pool import (
    EndpointNodeConfig,
    EndpointPool,
    HealthObservation,
    HealthPolicy,
    HealthSignal,
    RequestKind,
)
from dcg.sequencer.providers import RpcSendProvider
from dcg.sequencer.types import AccountInfo, BlockhashLease, RpcEndpoint, SendReceipt


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_RUN = ROOT / "out/runs/doom-dcg-p2-stopgap-2026-10-01"
DEFAULT_IMAGE_DIR = DEFAULT_RUN / "input"
DEFAULT_RESOURCE = DEFAULT_IMAGE_DIR / "doom-dcg-resource.bin"
DEFAULT_COMMANDS = Path(__file__).resolve().parent / "tests/fixtures/commands.bin"
RESOURCE_KEY = Pubkey.from_bytes(bytes([0xD0]) * 32)
DOOM_RESOURCE_ROOT = bytes.fromhex("26cc68c19943535ee09101690780ffb35692a5e4aef2c0e05467d02fff97a53e")
DOOM_KERNEL_ID = b"basanos-doom-v3\0"
STATE_SCHEMA_ID = 0x4D4F4F44
RESOURCE_SCHEMA_ID = 0x53524744
MODE_CONSENSUS_V3 = 0x434F4E53
WIRE_VERSION = 3
SYSTEM_PROGRAM = Pubkey.default()
COMPUTE_LIMIT = 1_400_000
# DCG's stateful kernel declaration is below the transaction CU limit by the
# adapter overhead and safety margin pinned in the bundled r7c image.
KERNEL_COMPUTE_CEILING = COMPUTE_LIMIT - 45_573 - 25_000
CHILD_GROW_BYTES = 8_192
RESOURCE_CHUNK_BYTES = 65_536
STREAM_CAPACITY = 512
STATE_LENGTHS = (9_734_160, 126_976, 65_536)
RESOURCE_BYTES = 4_457_108
SNAPSHOT_BYTES = 1_048_768
STRIP_BYTES = 8_128
VIEW_PHASE_BYTES = STRIP_BYTES
VIEW_WORKSPACE_BYTES = 10_462_608
SNAPSHOT_ABI = bytes([0x52]) * 32
STRIP_ABIS = tuple(bytes([0x53 + index]) * 32 for index in range(8))
FRAME_BYTES = SNAPSHOT_BYTES + STRIP_BYTES * 8
SNAPSHOT_BOUNDARY = SNAPSHOT_BYTES // VIEW_PHASE_BYTES * VIEW_PHASE_BYTES
FRAME_PIXELS_BYTES = 320 * 200
PIXEL_OFFSET_IN_STRIP = 256
STRIP_WIDTH = 40
FRAME_HEIGHT = 200
MAX_PHASES_PER_TX = 13
TX_PACKET_LIMIT = 1_232
ENDPOINT_ID = "doom-rpc-pool"
TESTNET_NODE_URLS = (
    "http://64.130.44.173:8899",
    "http://212.126.58.17:8899",
    "http://206.125.33.22:8899",
    "http://64.130.37.72:8899",
    "http://64.130.63.165:8899",
    "http://64.130.44.172:8899",
)
TESTNET_GENESIS = "9GGSFo95u8MAtJGn2peX7v6GkBX5doWvNQSDD4mG2sQT"


def sha256(data: bytes) -> bytes:
    return hashlib.sha256(data).digest()


def u16(value: int) -> bytes:
    return value.to_bytes(2, "little")


def u32(value: int) -> bytes:
    return value.to_bytes(4, "little")


def u64(value: int) -> bytes:
    return value.to_bytes(8, "little")


def _pda(program: Pubkey, *seeds: bytes) -> Pubkey:
    return Pubkey.find_program_address(list(seeds), program)[0]


@dataclass(frozen=True)
class Addresses:
    session: Pubkey
    stream: Pubkey
    states: tuple[Pubkey, Pubkey, Pubkey]
    resource: Pubkey
    views: tuple[Pubkey, ...]
    workspace: Pubkey
    scratch: Pubkey

    @classmethod
    def derive(cls, program: Pubkey, authority: Pubkey, session_id: int) -> "Addresses":
        session = _pda(program, b"dcg-session-v3", bytes(authority), u64(session_id))
        return cls(
            session=session,
            stream=_pda(program, b"dcg-input-v3", bytes(session)),
            states=tuple(
                _pda(program, b"dcg-state-v3", bytes(session), bytes([index]))
                for index in range(3)
            ),
            resource=_pda(program, b"dcg-resource-v3", bytes(session)),
            views=tuple(
                _pda(program, b"dcg-view-v3", bytes(session), bytes([role]))
                for role in range(9)
            ),
            workspace=_pda(program, b"dcg-view-v3", bytes(session), bytes([254])),
            scratch=_pda(program, b"dcg-view-v3", bytes(session), bytes([255])),
        )


def ix(program: Pubkey, tag: int, body: bytes, accounts: Iterable[tuple[Pubkey, bool, bool]]) -> Instruction:
    metas = [AccountMeta(key, signer, writable) for key, signer, writable in accounts]
    return Instruction(program, bytes([tag]) + body, metas)


def open_session(program: Pubkey, payer: Pubkey, authority: Pubkey, addresses: Addresses, session_id: int) -> Instruction:
    body = bytearray([WIRE_VERSION])
    body.extend(u64(session_id))
    body.extend(bytes([0, 4]))  # indexed input, 4-byte Doom command
    body.extend(u32(STREAM_CAPACITY))
    body.append(1)  # one tic per ADVANCE
    body.extend(DOOM_KERNEL_ID)
    body.extend(u16(3))  # semantic version
    body.extend(u16(1))  # ABI version
    body.extend(u32(MODE_CONSENSUS_V3))
    body.extend(u16(3))  # mode version
    body.extend(bytes([0xA5]) * 32)  # stream root
    body.extend(bytes(32))  # writer defaults to authority
    body.extend(bytes(RESOURCE_KEY))
    body.extend(u32(RESOURCE_SCHEMA_ID))
    body.extend(u16(2))
    body.extend(DOOM_RESOURCE_ROOT)
    body.append(1)  # headerless primary state at span zero
    return ix(program, 230, bytes(body), (
        (payer, True, True),
        (authority, True, False),
        (addresses.session, False, True),
        (RESOURCE_KEY, False, False),
        (addresses.resource, False, True),
        (SYSTEM_PROGRAM, False, False),
    ))


def create_stream(program: Pubkey, payer: Pubkey, a: Addresses) -> Instruction:
    return ix(program, 231, bytes([WIRE_VERSION, 0]), (
        (payer, True, True), (a.session, False, True), (a.stream, False, True),
        (SYSTEM_PROGRAM, False, False),
    ))


def create_state(program: Pubkey, payer: Pubkey, a: Addresses) -> Instruction:
    body = bytearray([WIRE_VERSION, len(STATE_LENGTHS)])
    for length in STATE_LENGTHS:
        body.extend(u32(length))
    return ix(program, 232, bytes(body), (
        (payer, True, True), (a.session, False, True), (a.resource, False, False),
        (a.states[0], False, True), (a.states[1], False, True), (a.states[2], False, True),
        (SYSTEM_PROGRAM, False, False),
    ))


def resource_grow(program: Pubkey, payer: Pubkey, authority: Pubkey, a: Addresses, target: int) -> Instruction:
    return ix(program, 240, bytes([WIRE_VERSION, 0xFE]) + u32(target), (
        (payer, True, True), (authority, True, False), (a.session, False, True),
        (a.resource, False, True), (SYSTEM_PROGRAM, False, False),
    ))


def resource_leaf(index: int, resource: bytes) -> bytes:
    chunk = resource[index * RESOURCE_CHUNK_BYTES:(index + 1) * RESOURCE_CHUNK_BYTES]
    return sha256(b"dcg/resource-chunk/1" + u32(index) + u32(len(resource)) + chunk)


def resource_tree(resource: bytes) -> list[list[bytes]]:
    leaves = [resource_leaf(i, resource) for i in range(math.ceil(len(resource) / RESOURCE_CHUNK_BYTES))]
    levels = [leaves]
    while len(levels[-1]) > 1:
        current = levels[-1]
        nxt = []
        for at in range(0, len(current), 2):
            right = current[at + 1] if at + 1 < len(current) else current[at]
            nxt.append(sha256(b"dcg/resource-node/1" + current[at] + right))
        levels.append(nxt)
    return levels


def resource_proof(levels: Sequence[Sequence[bytes]], index: int) -> bytes:
    proof = bytearray()
    at = index
    for level in levels[:-1]:
        sibling = at ^ 1
        if sibling < len(level):
            proof.extend(level[sibling])
        at >>= 1
    return bytes(proof)


def resource_upload(program: Pubkey, authority: Pubkey, a: Addresses, index: int, proof: bytes) -> Instruction:
    body = bytes([WIRE_VERSION]) + u32(index) + bytes([len(proof) // 32]) + proof
    return ix(program, 240, body, (
        (authority, True, False), (a.session, False, True),
        (a.resource, False, True), (RESOURCE_KEY, False, False),
    ))


def grow_state(program: Pubkey, payer: Pubkey, a: Addresses, index: int) -> Instruction:
    return ix(program, 232, bytes([WIRE_VERSION, 0xFE, index]), (
        (payer, True, True), (a.session, False, False),
        (a.states[index], False, True), (SYSTEM_PROGRAM, False, False),
    ))


def begin_initialization(program: Pubkey, authority: Pubkey, a: Addresses) -> Instruction:
    total = sum(STATE_LENGTHS)
    return ix(program, 232, bytes([WIRE_VERSION, 0xFC]) + u32(total) + u32(KERNEL_COMPUTE_CEILING), (
        (authority, True, False), (a.session, False, True), (a.resource, False, False),
    ))


def run_initialization(program: Pubkey, authority: Pubkey, a: Addresses, cursor: int) -> Instruction:
    return ix(program, 232, bytes([WIRE_VERSION, 0xFD]) + u32(cursor) + u32(KERNEL_COMPUTE_CEILING), (
        (a.states[0], False, True), (authority, True, False), (a.session, False, True),
        (a.resource, False, False), (a.states[1], False, True), (a.states[2], False, True),
    ))


def write_input(program: Pubkey, authority: Pubkey, a: Addresses, sequence: int, command: bytes) -> Instruction:
    return ix(program, 234, bytes([WIRE_VERSION]) + u32(sequence) + bytes([len(command)]) + command, (
        (authority, True, False), (a.session, False, True), (a.stream, False, True),
    ))


def advance(program: Pubkey, authority: Pubkey, a: Addresses, cursor: int) -> Instruction:
    return ix(program, 235, bytes([WIRE_VERSION]) + u32(cursor) + bytes([1]), (
        (a.states[0], False, True), (authority, True, False), (a.session, False, True),
        (a.stream, False, True), (a.states[1], False, True), (a.states[2], False, True),
    ))


def create_view(program: Pubkey, payer: Pubkey, a: Addresses, role: int, abi: bytes, length: int) -> Instruction:
    body = bytes([WIRE_VERSION, role]) + abi + u32(0) + u32(length)
    address = a.views[role] if role < 9 else a.workspace if role == 254 else a.scratch
    return ix(program, 233, body, (
        (payer, True, True), (a.session, False, True), (address, False, True),
        (SYSTEM_PROGRAM, False, False),
    ))


def create_workspace(program: Pubkey, payer: Pubkey, a: Addresses) -> Instruction:
    return ix(program, 233, bytes([WIRE_VERSION, 254]) + u32(VIEW_WORKSPACE_BYTES), (
        (payer, True, True), (a.session, False, True), (a.workspace, False, True),
        (SYSTEM_PROGRAM, False, False),
    ))


def create_scratch(program: Pubkey, payer: Pubkey, a: Addresses) -> Instruction:
    body = bytes([WIRE_VERSION, 255]) + bytes(32) + u32(0) + u32(FRAME_BYTES)
    return ix(program, 233, body, (
        (payer, True, True), (a.session, False, True), (a.scratch, False, True),
        (SYSTEM_PROGRAM, False, False),
    ))


def grow_view(program: Pubkey, payer: Pubkey, a: Addresses, role: int) -> Instruction:
    address = a.views[role] if role < 9 else a.workspace if role == 254 else a.scratch
    return ix(program, 233, bytes([WIRE_VERSION, 0xFE, role]), (
        (payer, True, True), (a.session, False, True), (address, False, True),
        (SYSTEM_PROGRAM, False, False),
    ))


def publication_accounts(program: Pubkey, authority: Pubkey, a: Addresses, *, outputs_writable: bool,
                         workspace_writable: bool, scratch_writable: bool) -> list[AccountMeta]:
    metas = [
        AccountMeta(a.states[0], False, False),
        AccountMeta(authority, True, False),
        AccountMeta(a.session, False, True),
        AccountMeta(a.resource, False, False),
        AccountMeta(a.states[1], False, False),
        AccountMeta(a.states[2], False, False),
    ]
    metas.extend(AccountMeta(key, False, outputs_writable) for key in a.views)
    metas.extend((AccountMeta(a.workspace, False, workspace_writable), AccountMeta(a.scratch, False, scratch_writable)))
    return metas


def workspace_first_accounts(authority: Pubkey, a: Addresses) -> list[AccountMeta]:
    return [
        AccountMeta(a.workspace, False, True), AccountMeta(authority, True, False),
        AccountMeta(a.session, False, True), AccountMeta(a.resource, False, False),
        *(AccountMeta(key, False, False) for key in a.states),
        *(AccountMeta(key, False, False) for key in a.views),
        AccountMeta(a.scratch, False, True),
    ]


def begin_views(program: Pubkey, authority: Pubkey, a: Addresses, cursor: int) -> Instruction:
    body = bytes([WIRE_VERSION, 0]) + u32(cursor) + u32(KERNEL_COMPUTE_CEILING)
    return Instruction(program, bytes([236]) + body,
                       publication_accounts(program, authority, a, outputs_writable=False,
                                            workspace_writable=True, scratch_writable=True))


def run_view_phase(program: Pubkey, authority: Pubkey, a: Addresses, cursor: int, workspace_first: bool) -> Instruction:
    body = bytes([WIRE_VERSION, 1]) + u32(cursor) + u32(KERNEL_COMPUTE_CEILING)
    if workspace_first:
        return Instruction(program, bytes([236]) + body, workspace_first_accounts(authority, a))
    return Instruction(program, bytes([236]) + body,
                       publication_accounts(program, authority, a, outputs_writable=False,
                                            workspace_writable=True, scratch_writable=True))


def commit_views(program: Pubkey, authority: Pubkey, a: Addresses, cursor: int) -> Instruction:
    body = bytes([WIRE_VERSION, 2]) + u32(cursor) + u32(FRAME_BYTES) + u32(KERNEL_COMPUTE_CEILING)
    return Instruction(program, bytes([236]) + body,
                       publication_accounts(program, authority, a, outputs_writable=True,
                                            workspace_writable=False, scratch_writable=False))


def halt_session(program: Pubkey, authority: Pubkey, a: Addresses, cursor: int) -> Instruction:
    return ix(program, 237, bytes([WIRE_VERSION]) + u32(cursor), (
        (authority, True, False), (a.session, False, True),
    ))


def close_child(program: Pubkey, a: Addresses, target: Pubkey, refund: Pubkey, kind: int) -> Instruction:
    return ix(program, 238, bytes([WIRE_VERSION, kind]), (
        (a.session, False, True), (target, False, True), (refund, False, True),
    ))


def close_session(program: Pubkey, a: Addresses, refund: Pubkey) -> Instruction:
    return ix(program, 238, bytes([WIRE_VERSION, 0]), (
        (a.session, False, True), (refund, False, True),
    ))


class MultiKeypairSigner:
    """Small app adapter for DCG's signer protocol and payer+authority txs."""

    def __init__(self, keypair_paths: Sequence[str]):
        self.keypairs = [Keypair.from_bytes(bytes(json.loads(Path(path).read_text()))) for path in keypair_paths]
        self.by_pubkey = {str(keypair.pubkey()): keypair for keypair in self.keypairs}
        self._payer = self.keypairs[0]

    @property
    def public_key(self) -> str:
        return str(self._payer.pubkey())

    @property
    def signature_count(self) -> int:
        return 2  # conservative sizing; most transactions require one signer

    @property
    def signature_size_bytes(self) -> int:
        return 64

    async def sign(self, message_bytes: bytes, lease: BlockhashLease) -> SignedTransaction:
        message = Message.from_bytes(message_bytes)
        signatures = []
        required = message.header.num_required_signatures
        for pubkey in message.account_keys[:required]:
            try:
                keypair = self.by_pubkey[str(pubkey)]
            except KeyError as exc:
                raise ValueError(f"no local signer for required transaction key {pubkey}") from exc
            signatures.append(keypair.sign_message(message_bytes))
        tx = Transaction.populate(message, signatures)
        return SignedTransaction(str(signatures[0]), bytes(tx))


class MeasuringRpcEndpoint(SolanaRpcEndpoint):
    async def get_transaction_metadata(self, signature: str) -> dict[str, Any] | None:
        """Read Fogo's standard transaction metadata through the pooled lease."""
        result = await self._call("getTransaction", [
            signature,
            {"encoding": "json", "commitment": "confirmed", "maxSupportedTransactionVersion": 0},
        ])
        return result if isinstance(result, dict) else None


class PooledRpcEndpoint:
    """Single sequencer endpoint backed by package A's pool and RPC provider."""

    def __init__(self, pool: EndpointPool, nodes: dict[str, MeasuringRpcEndpoint], receipts_path: Path):
        self.pool = pool
        self.nodes = nodes
        self.endpoint_id = ENDPOINT_ID
        self.provider = RpcSendProvider(nodes, provider_id="rpc")
        self.receipts_path = receipts_path
        self.counts: dict[str, dict[str, int]] = {
            key: {"send_attempts": 0, "send_accepted": 0, "rpc_requests": 0,
                  "429": 0, "failures": 0, "health_probes": 0}
            for key in nodes
        }

    async def _run(self, kind: RequestKind, method: str, *args: Any,
                   pool_endpoint_id: str | None = None, **kwargs: Any) -> tuple[str, Any]:
        while True:
            lease = await self.pool.acquire(kind, endpoint_id=pool_endpoint_id)
            endpoint_id = lease.endpoint_id
            endpoint = lease.endpoint
            if lease.route.is_probe:
                self.counts[endpoint_id]["health_probes"] += 1
                try:
                    await endpoint.get_health()
                except Exception as exc:
                    observation = classify(exc)
                    await lease.observe(observation)
                    await lease.close()
                    continue
                await lease.observe(HealthObservation(HealthSignal.SUCCESS))
                await lease.close()
                continue
            self.counts[endpoint_id]["rpc_requests"] += 1
            started = time.monotonic()
            try:
                result = await getattr(endpoint, method)(*args, **kwargs)
            except Exception as exc:
                try:
                    observation = classify(exc)
                except TypeError:
                    await lease.close()
                    raise exc
                self.counts[endpoint_id]["failures"] += 1
                if observation.signal is HealthSignal.RATE_LIMITED:
                    self.counts[endpoint_id]["429"] += 1
                self._append_jsonl(self.receipts_path, {
                    "kind": "rpc_error", "endpoint_id": endpoint_id, "method": method,
                    "error_class": type(exc).__name__, "error": str(exc)[:256], "at_unix": time.time(),
                })
                await lease.observe(observation)
                await lease.close()
                raise
            await lease.observe(HealthObservation(
                HealthSignal.SUCCESS, latency_seconds=time.monotonic() - started
            ))
            await lease.close()
            self._append_jsonl(self.receipts_path, {
                "kind": "rpc", "endpoint_id": endpoint_id, "method": method,
                "latency_seconds": time.monotonic() - started, "at_unix": time.time(),
            })
            return endpoint_id, result

    async def verify_nodes(self, expected_genesis: str | None = None) -> str:
        observed = []
        for endpoint_id, endpoint in self.nodes.items():
            selected, genesis = await self._run(RequestKind.RPC, "get_genesis_hash", pool_endpoint_id=endpoint_id)
            observed.append(genesis)
            if expected_genesis is not None and genesis != expected_genesis:
                raise RuntimeError(f"RPC {selected} genesis mismatch: {genesis}")
        if len(set(observed)) != 1:
            raise RuntimeError(f"RPC pool spans different genesis hashes: {observed}")
        return observed[0]

    async def latest_blockhash(self, genesis_hash: str, lifetime_seconds: float) -> Any:
        _node, result = await self._run(RequestKind.RPC, "latest_blockhash", genesis_hash, lifetime_seconds)
        return result

    async def send_raw_transaction(self, raw_bytes: bytes) -> SendReceipt:
        signature = str(Signature.from_bytes(raw_bytes[1:65]))
        while True:
            lease = await self.pool.acquire(RequestKind.SEND)
            endpoint_id = lease.endpoint_id
            if lease.route.is_probe:
                self.counts[endpoint_id]["health_probes"] += 1
                try:
                    await lease.endpoint.get_health()
                except Exception as exc:
                    await lease.observe(classify(exc))
                    await lease.close()
                    continue
                await lease.observe(HealthObservation(HealthSignal.SUCCESS))
                await lease.close()
                continue
            self.counts[endpoint_id]["send_attempts"] += 1
            self.counts[endpoint_id]["rpc_requests"] += 1
            try:
                receipt = await self.provider.send_raw(raw_bytes, signature, lease.route)
            except Exception as exc:
                try:
                    observation = classify(exc)
                except TypeError:
                    await lease.close()
                    raise exc
                self.counts[endpoint_id]["failures"] += 1
                if observation.signal is HealthSignal.RATE_LIMITED:
                    self.counts[endpoint_id]["429"] += 1
                self._append_jsonl(self.receipts_path, {
                    "kind": "send_error", "endpoint_id": endpoint_id,
                    "error_class": type(exc).__name__, "signature": signature,
                    "error": str(exc)[:256],
                    "at_unix": time.time(),
                })
                await lease.observe(observation)
                await lease.close()
                raise
            await lease.observe(HealthObservation(
                HealthSignal.SUCCESS, latency_seconds=max(0.0, time.time() - receipt.accepted_at_unix)
            ))
            await lease.close()
            self.counts[endpoint_id]["send_accepted"] += 1
            self._append_jsonl(self.receipts_path, {
                "kind": "send_acknowledged", "signature": receipt.signature,
                "endpoint_id": receipt.endpoint_id, "provider_id": receipt.provider_id,
                "accepted_at_unix": receipt.accepted_at_unix,
            })
            return SendReceipt(receipt.signature)

    async def signature_status(self, signature: str) -> Any:
        _node, result = await self._run(RequestKind.RPC, "signature_status", signature)
        return result

    async def get_multiple_accounts(self, addresses: Sequence[str], commitment: Commitment) -> Any:
        _node, result = await self._run(RequestKind.RPC, "get_multiple_accounts", addresses, commitment)
        return result

    async def get_program_accounts(self, program_id: str, *, filters: Sequence[dict[str, object]], commitment: Commitment) -> Any:
        _node, result = await self._run(RequestKind.RPC, "get_program_accounts", program_id,
                                        filters=filters, commitment=commitment)
        return result

    async def simulate_transaction(self, raw_bytes: bytes, *, commitment: Commitment = Commitment.CONFIRMED,
                                   sig_verify: bool = False, replace_recent_blockhash: bool = False) -> Any:
        _node, result = await self._run(RequestKind.RPC, "simulate_transaction", raw_bytes,
                                        commitment=commitment, sig_verify=sig_verify,
                                        replace_recent_blockhash=replace_recent_blockhash)
        return result

    async def get_transaction_metadata(self, signature: str) -> dict[str, Any] | None:
        _node, result = await self._run(RequestKind.RPC, "get_transaction_metadata", signature)
        return result

    @staticmethod
    def _append_jsonl(path: Path, row: dict[str, Any]) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("a", encoding="utf-8") as output:
            output.write(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n")
            output.flush()
            os.fsync(output.fileno())


def load_config(run_dir: Path) -> dict[str, Any]:
    path = run_dir / "driver-config.json"
    if not path.exists():
        raise RuntimeError(f"missing driver config: {path}; run prepare first")
    return json.loads(path.read_text())


def write_keypair(path: Path) -> Keypair:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        raise FileExistsError(f"refusing to overwrite keypair {path}")
    keypair = Keypair()
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as output:
        json.dump(list(bytes(keypair)), output)
        output.write("\n")
    os.chmod(path, stat.S_IRUSR | stat.S_IWUSR)
    return keypair


def prepare(args: argparse.Namespace) -> None:
    run_dir: Path = args.run_dir
    run_dir.mkdir(parents=True, exist_ok=True)
    config_path = run_dir / "driver-config.json"
    if config_path.exists():
        raise RuntimeError(f"refusing to replace existing run config: {config_path}")
    if args.network == "testnet":
        key_dir = ROOT / "out/chain/doom-dcg-p2-stopgap-2026-10-01"
        urls = list(TESTNET_NODE_URLS)
        expected = TESTNET_GENESIS
    else:
        key_dir = run_dir / "keys"
        urls = [args.rpc_url]
        expected = None
    names = ("payer", "authority", "doom-program", "dcg-program")
    key_paths: dict[str, str] = {}
    pubkeys: dict[str, str] = {}
    for name in names:
        path = key_dir / f"{name}.json"
        keypair = write_keypair(path)
        key_paths[name] = str(path)
        pubkeys[name] = str(keypair.pubkey())
    config = {
        "schema": 1,
        "network": args.network,
        "rpc_urls": urls,
        "expected_genesis": expected,
        "key_paths": key_paths,
        "pubkeys": pubkeys,
        "program_id": pubkeys["doom-program"],
        "dcg_program_id": pubkeys["dcg-program"],
        "session_id": secrets.randbits(63),
        "resource_key": str(RESOURCE_KEY),
        "resource_path": str(DEFAULT_RESOURCE.resolve()),
        "commands_path": str(DEFAULT_COMMANDS.resolve()),
    }
    config_path.write_text(json.dumps(config, sort_keys=True, indent=2) + "\n")
    os.chmod(config_path, 0o600)
    if args.network == "local":
        resource = Path(config["resource_path"]).read_bytes()
        if len(resource) != RESOURCE_BYTES or resource_tree(resource)[-1][0] != DOOM_RESOURCE_ROOT:
            raise RuntimeError("local resource differs from the pinned Doom bundle")
        genesis_account = {
            "pubkey": str(RESOURCE_KEY),
            "account": {
                "lamports": 1_000_000_000_000,
                "data": [base64.b64encode(resource).decode("ascii"), "base64"],
                "owner": config["program_id"],
                "executable": False,
                "rentEpoch": 0,
                "space": len(resource),
            },
        }
        account_path = run_dir / "resource-source-account.json"
        account_path.write_text(json.dumps(genesis_account, separators=(",", ":")) + "\n")
        os.chmod(account_path, 0o600)
    print(json.dumps({"run_dir": str(run_dir), "network": args.network,
                      "public_ids": pubkeys, "config": str(config_path)}, sort_keys=True))


def make_pool(config: dict[str, Any], *, node_count: int, send_rate: float, request_rate: float,
              max_in_flight: int, run_dir: Path) -> tuple[EndpointPool, dict[str, MeasuringRpcEndpoint], PooledRpcEndpoint]:
    urls = config["rpc_urls"]
    if not 1 <= node_count <= len(urls):
        raise ValueError(f"node count must be in 1..{len(urls)} for the selected network")
    nodes: dict[str, MeasuringRpcEndpoint] = {}
    configs = []
    for index, url in enumerate(urls[:node_count]):
        endpoint_id = f"fogo-rpc-{index + 1}"
        endpoint = MeasuringRpcEndpoint(endpoint_id, url, config=RpcConfig(
            timeout_seconds=8,
            requests_per_second=10_000,
            max_in_flight=max(16, max_in_flight * 2),
            commitment=Commitment.CONFIRMED,
        ))
        nodes[endpoint_id] = endpoint
        configs.append(EndpointNodeConfig(
            endpoint=endpoint, sends_per_second=send_rate,
            requests_per_second=request_rate, max_in_flight=max_in_flight,
        ))
    pool = EndpointPool(configs, health_policy=HealthPolicy(
        score_threshold=3.0,
        cooldown_seconds=1.0,
        max_cooldown_seconds=60.0,
        decay_half_life_seconds=3_600.0,
        recovery_points=0.0,
        rate_limit_points=3.0,
        transport_error_points=1.0,
        timeout_points=1.0,
        unhealthy_points=1.0,
    ), default_acquire_timeout_seconds=90.0)
    return pool, nodes, PooledRpcEndpoint(pool, nodes, run_dir / "send-receipts.jsonl")


def make_sequencer(config: dict[str, Any], endpoints: PooledRpcEndpoint, key_paths: dict[str, str],
                   *, send_rate: float, max_in_flight: int, event_hook=None) -> Sequencer:
    signer = MultiKeypairSigner((key_paths["payer"], key_paths["authority"]))
    return Sequencer(
        endpoints={ENDPOINT_ID: endpoints}, signer=signer,
        config=SequencerConfig(
            endpoint_limits={ENDPOINT_ID: EndpointLimits(sends_per_second=10_000,
                                                         max_in_flight=max(16, max_in_flight))},
            max_batch_size=32,
            max_packet_bytes=TX_PACKET_LIMIT,
            blockhash_lifetime_seconds=60.0,
            per_step_time_cap_seconds=90.0,
            confirmation_poll_seconds=0.2,
            backoff=Backoff(initial_seconds=1.0, maximum_seconds=60.0, multiplier=2.0),
        ), event_hook=event_hook,
    )


def tx_digest(program: Pubkey, instructions: Sequence[Instruction]) -> str:
    out = bytearray(bytes(program))
    out.extend(set_compute_unit_limit(COMPUTE_LIMIT).data)
    for instruction in instructions:
        out.extend(bytes(instruction.program_id))
        out.extend(len(instruction.data).to_bytes(4, "little"))
        out.extend(bytes(instruction.data))
        for meta in instruction.accounts:
            out.extend(bytes(meta.pubkey))
            out.extend(bytes([meta.is_signer, meta.is_writable]))
    return hashlib.sha256(out).hexdigest()


def make_step(*, program: Pubkey, payer: Pubkey, instructions: Sequence[Instruction], step_id: str,
              dependencies: tuple[str, ...], locks: Iterable[Pubkey], compute_class: str,
              postcondition=None) -> TransactionStep:
    locks_tuple = tuple(sorted({str(key) for key in locks}))
    digest = tx_digest(program, instructions)
    recovery = hashlib.sha256(("doom-p2-postcondition-v1:" + step_id).encode()).hexdigest()

    def build(lease: BlockhashLease) -> bytes:
        tx_message = Message.new_with_blockhash(
            [set_compute_unit_limit(COMPUTE_LIMIT), *instructions], payer,
            Hash.from_string(lease.blockhash),
        )
        return bytes(tx_message)

    async def check(endpoint: RpcEndpoint) -> PostconditionResult:
        if postcondition is None:
            return PostconditionResult(None)
        return await postcondition(endpoint)

    return TransactionStep(
        step_id=step_id, dependencies=dependencies, endpoint_id=ENDPOINT_ID,
        compute_class=compute_class, compute_unit_limit=COMPUTE_LIMIT,
        intent_digest=digest, recovery_policy_digest=recovery, build_message=build,
        postcondition=check, max_packet_bytes=TX_PACKET_LIMIT,
        write_locks=locks_tuple, retry_policy=RetryPolicy.SAME_BYTES,
    )


def exists_check(addresses: Sequence[Pubkey]):
    async def check(endpoint: RpcEndpoint) -> PostconditionResult:
        infos = await endpoint.get_multiple_accounts([str(x) for x in addresses], Commitment.CONFIRMED)
        return PostconditionResult(all(info is not None for info in infos))
    return check


def cursor_check(session: Pubkey, minimum: int):
    async def check(endpoint: RpcEndpoint) -> PostconditionResult:
        infos = await endpoint.get_multiple_accounts([str(session)], Commitment.CONFIRMED)
        info = infos[0]
        if info is None or len(info.data) < 116:
            return PostconditionResult(False)
        cursor = int.from_bytes(info.data[112:116], "little")
        return PostconditionResult(cursor >= minimum, hashlib.sha256(info.data[112:116]).hexdigest())
    return check


def _append_step(steps: list[TransactionStep], *, program: Pubkey, payer: Pubkey,
                 instructions: Sequence[Instruction], name: str, locks: Iterable[Pubkey],
                 compute_class: str, postcondition=None) -> None:
    dependencies = (steps[-1].step_id,) if steps else ()
    steps.append(make_step(program=program, payer=payer, instructions=instructions,
                           step_id=name, dependencies=dependencies, locks=locks,
                           compute_class=compute_class, postcondition=postcondition))


def packet_safe_batches(program: Pubkey, payer: Pubkey,
                        instructions: Sequence[Instruction]) -> list[list[Instruction]]:
    """Pack deterministic instruction batches under the serialized packet cap."""
    compute_budget = set_compute_unit_limit(COMPUTE_LIMIT)

    def projected(batch: Sequence[Instruction]) -> int:
        message = Message.new_with_blockhash(
            [compute_budget, *batch], payer, Hash.default()
        )
        # Payer and authority are the maximum signers used by this plan.
        return len(bytes(message)) + 1 + 2 * 64

    batches: list[list[Instruction]] = []
    current: list[Instruction] = []
    for instruction in instructions:
        candidate = [*current, instruction]
        if projected(candidate) <= TX_PACKET_LIMIT:
            current = candidate
            continue
        if not current:
            raise ValueError(
                f"single {instruction.program_id} instruction projects beyond {TX_PACKET_LIMIT} bytes"
            )
        batches.append(current)
        current = [instruction]
        if projected(current) > TX_PACKET_LIMIT:
            raise ValueError(
                f"single {instruction.program_id} instruction projects beyond {TX_PACKET_LIMIT} bytes"
            )
    if current:
        batches.append(current)
    return batches


def setup_plan(config: dict[str, Any], resource: bytes) -> tuple[TransactionPlan, Addresses]:
    program = Pubkey.from_string(config["program_id"])
    payer = Pubkey.from_string(config["pubkeys"]["payer"])
    authority = Pubkey.from_string(config["pubkeys"]["authority"])
    a = Addresses.derive(program, authority, config["session_id"])
    steps: list[TransactionStep] = []
    _append_step(steps, program=program, payer=payer,
                 instructions=[open_session(program, payer, authority, a, config["session_id"])],
                 name="open-session", locks=(a.session, a.resource), compute_class="doom-open-session",
                 postcondition=exists_check((a.session,)))
    _append_step(steps, program=program, payer=payer, instructions=[create_stream(program, payer, a)],
                 name="create-stream", locks=(a.session, a.stream), compute_class="doom-create-stream",
                 postcondition=exists_check((a.stream,)))
    _append_step(steps, program=program, payer=payer, instructions=[create_state(program, payer, a)],
                 name="create-state-spans", locks=(a.session, a.resource, *a.states),
                 compute_class="doom-create-state", postcondition=exists_check(a.states))
    commands = Path(config["commands_path"]).read_bytes()
    command_width = 4
    _append_step(steps, program=program, payer=payer,
                 instructions=[write_input(program, authority, a, 0, commands[:command_width])],
                 name="write-input-000000", locks=(a.session, a.stream), compute_class="doom-write-input",
                 postcondition=None)

    allocated = CHILD_GROW_BYTES
    grow_ops = []
    while allocated < len(resource):
        allocated = min(allocated + CHILD_GROW_BYTES, len(resource))
        grow_ops.append(resource_grow(program, payer, authority, a, allocated))
    for batch_index, start in enumerate(range(0, len(grow_ops), 8)):
        batch = grow_ops[start:start + 8]
        _append_step(steps, program=program, payer=payer, instructions=batch,
                     name=f"resource-grow-{batch_index:04d}", locks=(a.session, a.resource),
                     compute_class="doom-resource-grow")
    levels = resource_tree(resource)
    chunks = len(levels[0])
    uploads = [resource_upload(program, authority, a, index, resource_proof(levels, index))
               for index in range(chunks)]
    for batch_index, batch in enumerate(packet_safe_batches(program, payer, uploads)):
        _append_step(steps, program=program, payer=payer, instructions=batch,
                     name=f"resource-upload-{batch_index:04d}", locks=(a.session, a.resource),
                     compute_class="doom-resource-upload")

    state_growth: list[Instruction] = []
    for index, desired in enumerate(STATE_LENGTHS):
        allocated = min(desired, CHILD_GROW_BYTES)
        while allocated < desired:
            allocated = min(allocated + CHILD_GROW_BYTES, desired)
            state_growth.append(grow_state(program, payer, a, index))
    for batch_index, start in enumerate(range(0, len(state_growth), 8)):
        batch = state_growth[start:start + 8]
        _append_step(steps, program=program, payer=payer, instructions=batch,
                     name=f"state-grow-{batch_index:04d}", locks=(a.session, *a.states),
                     compute_class="doom-state-grow")
    _append_step(steps, program=program, payer=payer,
                 instructions=[begin_initialization(program, authority, a)],
                 name="begin-initialization", locks=(a.session, a.resource, *a.states),
                 compute_class="doom-begin-initialization")
    phase_count = math.ceil(sum(STATE_LENGTHS) / 16_384)
    if phase_count != 606:
        raise AssertionError(f"unexpected Doom initialization phase count {phase_count}")
    for phase in range(phase_count):
        cursor = phase * 16_384
        _append_step(steps, program=program, payer=payer,
                     instructions=[run_initialization(program, authority, a, cursor)],
                     name=f"initialize-{phase:04d}", locks=(a.session, a.resource, *a.states),
                     compute_class="doom-initialization-phase")

    create_ops = [create_view(program, payer, a, 0, SNAPSHOT_ABI, SNAPSHOT_BYTES)]
    for role in range(1, 9):
        create_ops.append(create_view(program, payer, a, role, STRIP_ABIS[role - 1], STRIP_BYTES))
    create_ops.extend((create_workspace(program, payer, a), create_scratch(program, payer, a)))
    _append_step(steps, program=program, payer=payer, instructions=create_ops,
                 name="create-views-workspace", locks=(a.session, *a.views, a.workspace, a.scratch),
                 compute_class="doom-view-create", postcondition=exists_check((*a.views, a.workspace, a.scratch)))
    grow_view_ops: list[Instruction] = []
    for role, length in ((0, SNAPSHOT_BYTES), (254, VIEW_WORKSPACE_BYTES), (255, FRAME_BYTES)):
        allocated = min(length, CHILD_GROW_BYTES)
        while allocated < length:
            allocated = min(allocated + CHILD_GROW_BYTES, length)
            grow_view_ops.append(grow_view(program, payer, a, role))
    for batch_index, start in enumerate(range(0, len(grow_view_ops), 8)):
        batch = grow_view_ops[start:start + 8]
        _append_step(steps, program=program, payer=payer, instructions=batch,
                     name=f"view-grow-{batch_index:04d}", locks=(a.session, *a.views, a.workspace, a.scratch),
                     compute_class="doom-view-grow")
    plan = TransactionPlan(
        genesis_hash=config.get("observed_genesis") or config.get("expected_genesis") or "local-genesis-pending",
        program_id=str(program),
        destination_accounts=tuple(sorted({str(program), str(RESOURCE_KEY), str(a.session), str(a.stream),
                                            *(str(x) for x in a.states), str(a.resource),
                                            *(str(x) for x in a.views), str(a.workspace), str(a.scratch)})),
        signer_public_key=str(payer), steps=tuple(steps),
    )
    return plan, a


def campaign_plan(config: dict[str, Any], frames: int, start_cursor: int,
                  *, frame_offset: int = 0,
                  batch_final_strip_pair: bool = True,
                  prewritten_input_frames: frozenset[int] = frozenset()
                  ) -> tuple[TransactionPlan, Addresses, dict[str, int]]:
    if frames <= 0 or start_cursor < 0 or start_cursor % 8:
        raise ValueError("frames must be positive and start_cursor must be a nonnegative multiple of 8")
    program = Pubkey.from_string(config["program_id"])
    payer = Pubkey.from_string(config["pubkeys"]["payer"])
    authority = Pubkey.from_string(config["pubkeys"]["authority"])
    a = Addresses.derive(program, authority, config["session_id"])
    commands = Path(config["commands_path"]).read_bytes()
    if not commands or len(commands) % 4:
        raise ValueError("the captured command tape must contain whole four-byte Doom commands")
    command_count = len(commands) // 4
    steps: list[TransactionStep] = []
    commit_to_cursor: dict[str, int] = {}
    for frame in range(frames):
        cursor = start_cursor + frame * 8
        prefix = f"frame-{frame_offset + frame:04d}"
        if frame not in prewritten_input_frames:
            input_ixs = [write_input(program, authority, a, seq,
                                     commands[(seq % command_count) * 4:(seq % command_count + 1) * 4])
                         for seq in range(cursor + 1, cursor + 9)]
            _append_step(steps, program=program, payer=payer, instructions=input_ixs,
                         name=f"{prefix}-input-{cursor + 1:08d}-{cursor + 8:08d}",
                         locks=(a.session, a.stream), compute_class="doom-input-batch")
        for tic in range(8):
            at = cursor + tic
            _append_step(steps, program=program, payer=payer,
                         instructions=[advance(program, authority, a, at)],
                         name=f"{prefix}-advance-{at:08d}", locks=(a.session, a.stream, *a.states),
                         compute_class="doom-advance", postcondition=cursor_check(a.session, at + 1))
        target = cursor + 8
        _append_step(steps, program=program, payer=payer,
                     instructions=[begin_views(program, authority, a, target)],
                     name=f"{prefix}-snapshot-begin-{target:08d}",
                     locks=(a.session, *a.states, *a.views, a.workspace, a.scratch),
                     compute_class="doom-snapshot-begin")
        phase_cursor = 0
        while phase_cursor < SNAPSHOT_BOUNDARY:
            batch_count = min(MAX_PHASES_PER_TX, (SNAPSHOT_BOUNDARY - phase_cursor) // VIEW_PHASE_BYTES)
            if batch_count <= 0:
                break
            batch = [run_view_phase(program, authority, a, phase_cursor + i * VIEW_PHASE_BYTES, False)
                     for i in range(batch_count)]
            _append_step(steps, program=program, payer=payer, instructions=batch,
                         name=f"{prefix}-snapshot-{phase_cursor:08d}",
                         locks=(a.session, *a.states, *a.views, a.workspace, a.scratch),
                         compute_class="doom-snapshot-copy")
            phase_cursor += batch_count * VIEW_PHASE_BYTES
        while phase_cursor < FRAME_BYTES:
            remaining_phases = math.ceil((FRAME_BYTES - phase_cursor) / VIEW_PHASE_BYTES)
            # The retained r7c ProgramTest render groups its final two
            # workspace-first phases in one transaction (943,121 CU). Keep
            # the same static schedule while leaving earlier strip phases
            # individually bounded below the kernel ceiling. The legacy
            # single-phase schedule remains available to recover journals
            # created before that grouping was introduced.
            batch_count = (min(2, remaining_phases)
                           if batch_final_strip_pair and remaining_phases <= 2 else 1)
            _append_step(steps, program=program, payer=payer,
                         instructions=[run_view_phase(program, authority, a,
                                                      phase_cursor + i * VIEW_PHASE_BYTES, True)
                                       for i in range(batch_count)],
                         name=f"{prefix}-strip-phase-{phase_cursor:08d}",
                         locks=(a.session, *a.states, *a.views, a.workspace, a.scratch),
                         compute_class="doom-strip-render")
            phase_cursor += batch_count * VIEW_PHASE_BYTES
        commit_id = f"{prefix}-commit-{target:08d}"
        _append_step(steps, program=program, payer=payer,
                     instructions=[commit_views(program, authority, a, target)],
                     name=commit_id, locks=(a.session, *a.views, a.workspace, a.scratch),
                     compute_class="doom-frame-commit")
        commit_to_cursor[commit_id] = target
    plan = TransactionPlan(
        genesis_hash=config.get("observed_genesis") or config.get("expected_genesis") or "local-genesis-pending",
        program_id=str(program),
        destination_accounts=tuple(sorted({str(program), str(a.session), str(a.stream),
                                            *(str(x) for x in a.states), str(a.resource),
                                            *(str(x) for x in a.views), str(a.workspace), str(a.scratch)})),
        signer_public_key=str(payer), steps=tuple(steps),
    )
    return plan, a, commit_to_cursor


async def preflight(args: argparse.Namespace) -> None:
    config = load_config(args.run_dir)
    pool, nodes, endpoint = make_pool(config, node_count=args.node_count, send_rate=args.send_rate,
                                      request_rate=args.request_rate, max_in_flight=args.max_in_flight,
                                      run_dir=args.run_dir)
    try:
        genesis = await endpoint.verify_nodes(config.get("expected_genesis"))
        config["observed_genesis"] = genesis
        account = (await endpoint.get_multiple_accounts([str(RESOURCE_KEY)], Commitment.CONFIRMED))[0]
        payload = {"network": config["network"], "genesis_hash": genesis,
                   "resource_key": str(RESOURCE_KEY), "resource_present": account is not None,
                   "expected_program_owner": config["program_id"]}
        if account is not None:
            payload.update({"resource_owner": account.owner, "resource_executable": account.executable,
                            "resource_bytes": len(account.data), "resource_sha256": hashlib.sha256(account.data).hexdigest()})
        path = args.run_dir / "preflight.json"
        path.write_text(json.dumps(payload, sort_keys=True, indent=2) + "\n")
        if account is None:
            raise RuntimeError(
                f"pinned Doom source account {RESOURCE_KEY} is absent; the r7c image cannot initialize on this cluster"
            )
        if account.owner != config["program_id"] or account.executable:
            raise RuntimeError(
                f"pinned Doom source account owner/executable mismatch ({account.owner}, {account.executable})"
            )
        resource = Path(config["resource_path"]).read_bytes()
        if account.data != resource:
            raise RuntimeError("pinned Doom source account bytes differ from the supplied RI2/WAD resource")
        print(json.dumps(payload, sort_keys=True))
    finally:
        for node in nodes.values():
            await node.aclose()


async def deploy_one(args: argparse.Namespace) -> None:
    config = load_config(args.run_dir)
    if config["network"] != "testnet":
        raise RuntimeError("program deployment is available only for the testnet profile")
    if args.image_kind not in {"doom", "dcg"}:
        raise ValueError("image kind must be doom or dcg")
    # The source gate is deliberately before the first deployment transaction.
    pool, nodes, endpoint = make_pool(config, node_count=args.node_count, send_rate=args.send_rate,
                                      request_rate=args.request_rate, max_in_flight=args.max_in_flight,
                                      run_dir=args.run_dir)
    try:
        genesis = await endpoint.verify_nodes(TESTNET_GENESIS)
        source = (await endpoint.get_multiple_accounts([str(RESOURCE_KEY)], Commitment.CONFIRMED))[0]
        if source is None:
            raise RuntimeError(f"refusing deployment: required pinned resource source {RESOURCE_KEY} is absent")
        if source.owner != config["program_id"] or source.executable:
            raise RuntimeError("refusing deployment: pinned source account is not owned by this fresh Doom program ID")
    finally:
        for node in nodes.values():
            await node.aclose()
    image = DEFAULT_IMAGE_DIR / ("doom_dcg_r7c.so" if args.image_kind == "doom" else "dcg_program_r7c.so")
    program_key = config["key_paths"]["doom-program" if args.image_kind == "doom" else "dcg-program"]
    command = ["solana", "program", "deploy", "--url", TESTNET_NODE_URLS[0],
               "--use-rpc", "--commitment", "confirmed", "--max-sign-attempts", "1",
               "--keypair", config["key_paths"]["payer"], "--program-id", program_key, str(image)]
    subprocess.run(command, check=True)
    print(json.dumps({"deployed": args.image_kind, "program_id": config["pubkeys"]["doom-program" if args.image_kind == "doom" else "dcg-program"],
                      "genesis_hash": genesis, "image_sha256": hashlib.sha256(image.read_bytes()).hexdigest()}, sort_keys=True))


def account_fingerprint(info: AccountInfo | None) -> bytes:
    if info is None:
        return b"absent"
    return hashlib.sha256(info.owner.encode() + info.lamports.to_bytes(8, "little") + bytes([info.executable]) + info.data).digest()


async def run_plan(args: argparse.Namespace, plan: TransactionPlan, *, capture_frames: dict[str, int] | None = None,
                   addresses: Addresses | None = None) -> None:
    config = load_config(args.run_dir)
    pool, nodes, endpoint = make_pool(config, node_count=args.node_count, send_rate=args.send_rate,
                                      request_rate=args.request_rate, max_in_flight=args.max_in_flight,
                                      run_dir=args.run_dir)
    expected = config.get("expected_genesis")
    genesis = await endpoint.verify_nodes(expected)
    if config.get("observed_genesis") not in (None, genesis):
        raise RuntimeError("RPC genesis changed since preflight")
    plan = TransactionPlan(genesis_hash=genesis, program_id=plan.program_id,
                           destination_accounts=plan.destination_accounts,
                           signer_public_key=plan.signer_public_key, steps=plan.steps)
    journal_path = args.run_dir / "journals" / f"{args.journal}.jsonl"
    journal = JournalStore(journal_path)
    started_now = datetime.now(timezone.utc)
    commit_rows_path = args.run_dir / ("local-frame-hashes.jsonl" if config["network"] == "local" else "frame-results.jsonl")
    already = set()
    if commit_rows_path.exists():
        for line in commit_rows_path.read_text().splitlines():
            try:
                row = json.loads(line)
                already.add(row.get("step_id"))
            except json.JSONDecodeError:
                continue

    async def event_hook(event: str, step_id: str | None) -> None:
        if event != "step_confirmed" or not step_id or not capture_frames or step_id not in capture_frames:
            return
        if step_id in already:
            return
        assert addresses is not None
        target = capture_frames[step_id]
        infos = await endpoint.get_multiple_accounts([str(addresses.views[role]) for role in range(1, 9)],
                                                      Commitment.CONFIRMED)
        if any(info is None for info in infos):
            raise RuntimeError(f"rendered frame at cursor {target} is missing one or more strip accounts")
        strips = [info.data[PIXEL_OFFSET_IN_STRIP:PIXEL_OFFSET_IN_STRIP + STRIP_WIDTH * FRAME_HEIGHT]
                  for info in infos if info is not None]
        if any(len(strip) != STRIP_WIDTH * FRAME_HEIGHT for strip in strips):
            raise RuntimeError("committed strip payload has the wrong byte length")
        pixels = bytearray(FRAME_PIXELS_BYTES)
        offset = 0
        for row in range(FRAME_HEIGHT):
            for strip in strips:
                pixels[offset:offset + STRIP_WIDTH] = strip[row * STRIP_WIDTH:(row + 1) * STRIP_WIDTH]
                offset += STRIP_WIDTH
        frame_hash = hashlib.sha256(pixels).hexdigest()
        signature_rows: dict[str, str] = {}
        for journal_event in journal.events():
            if journal_event.name == "step_signed" and str(journal_event.data.get("step_id", "")).startswith(step_id.rsplit("-commit-", 1)[0]):
                signature_rows[str(journal_event.data.get("step_id"))] = str(journal_event.data.get("signature"))
        metadata = []
        for tx_step, signature in signature_rows.items():
            transaction = await endpoint.get_transaction_metadata(signature)
            meta = transaction.get("meta") if transaction else None
            metadata.append({"step_id": tx_step, "signature": signature,
                             "fee_lamports": meta.get("fee") if isinstance(meta, dict) else None,
                             "compute_units": meta.get("computeUnitsConsumed") if isinstance(meta, dict) else None})
        cus = [item["compute_units"] for item in metadata]
        fees = [item["fee_lamports"] for item in metadata]
        journal_events = journal.events()
        send_starts: dict[str, datetime] = {}
        confirmations: dict[str, datetime] = {}
        for journal_event in journal_events:
            event_step = journal_event.data.get("step_id")
            if not isinstance(event_step, str) or not event_step.startswith(step_id.rsplit("-commit-", 1)[0]):
                continue
            if journal_event.name == "send_attempt_started":
                send_starts.setdefault(event_step, datetime.fromisoformat(journal_event.recorded_at))
            elif journal_event.name == "step_confirmed":
                confirmations[event_step] = datetime.fromisoformat(journal_event.recorded_at)
        latency_ms = [
            (confirmed - send_starts[tx_step]).total_seconds() * 1000.0
            for tx_step, confirmed in confirmations.items() if tx_step in send_starts
        ]
        result = {
            "step_id": step_id, "frame_index": (target // 8) - 1, "cursor": target,
            "pixel_sha256": frame_hash, "transaction_count": len(metadata),
            "transactions": metadata,
            "compute_units_per_frame": sum(cus) if cus and all(isinstance(x, int) for x in cus) else None,
            "fee_lamports_per_frame": sum(fees) if fees and all(isinstance(x, int) for x in fees) else None,
            "landing_latency_ms": latency_ms,
            "recorded_at": datetime.now(timezone.utc).isoformat(),
        }
        golden_path = getattr(args, "golden", None)
        if golden_path:
            golden = json.loads(Path(golden_path).read_text())
            expected_hash = golden.get("frames", {}).get(str(target))
            result["golden_match"] = expected_hash == frame_hash if expected_hash is not None else None
            if expected_hash is not None and expected_hash != frame_hash:
                raise RuntimeError(f"frame {target} hash differs from local golden")
        PooledRpcEndpoint._append_jsonl(commit_rows_path, result)
        already.add(step_id)

    try:
        sequencer = make_sequencer(config, endpoint, config["key_paths"], send_rate=args.send_rate,
                                   max_in_flight=args.max_in_flight, event_hook=event_hook)
        if journal.events():
            await sequencer.resume(plan, journal)
        else:
            await sequencer.submit(plan, journal)
        if capture_frames:
            confirmed_commit_ids = {
                str(item.data.get("step_id")) for item in journal.events()
                if item.name == "step_confirmed"
            }
            for commit_id in capture_frames:
                if commit_id in confirmed_commit_ids and commit_id not in already:
                    # Reconstruct a frame receipt if the process was stopped
                    # after the commit confirmation but before its event hook
                    # finished. Campaign plans commit one frame at a time, so
                    # the current strip accounts still describe this frame.
                    await event_hook("step_confirmed", commit_id)
        journal_events = journal.events()
        run_start = next((datetime.fromisoformat(item.recorded_at)
                          for item in journal_events if item.name == "run_started"), started_now)
        run_end = datetime.now(timezone.utc)
        elapsed_seconds = max(0.0, (run_end - run_start).total_seconds())
        step_ids = {step.step_id for step in plan.steps}
        signed_events = [item for item in journal_events
                         if item.name == "step_signed" and item.data.get("step_id") in step_ids]
        signed_signatures = {str(item.data.get("signature")) for item in signed_events
                             if item.data.get("signature")}
        confirmed_steps = {str(item.data.get("step_id")) for item in journal_events
                           if item.name == "step_confirmed" and item.data.get("step_id") in step_ids}
        confirmed_signatures = {str(item.data.get("signature")) for item in journal_events
                                if item.name == "step_confirmed" and item.data.get("step_id") in step_ids
                                and item.data.get("signature")}
        send_attempts = [item for item in journal_events
                         if item.name == "send_attempt_started" and item.data.get("step_id") in step_ids]
        with (args.run_dir / "send-receipts.jsonl").open(encoding="utf-8") as source:
            send_receipts = [json.loads(line) for line in source if line.strip()]
        scoped_receipts = [item for item in send_receipts
                           if item.get("signature") in signed_signatures]
        accepted_by_signature: dict[str, str] = {}
        for item in scoped_receipts:
            if item.get("kind") == "send_acknowledged":
                accepted_by_signature[str(item["signature"])] = str(item["endpoint_id"])
        error_receipts = [item for item in scoped_receipts if item.get("kind") == "send_error"]
        frame_rows: list[dict[str, Any]] = []
        if capture_frames:
            with commit_rows_path.open(encoding="utf-8") as source:
                frame_rows = [json.loads(line) for line in source if line.strip()
                              and json.loads(line).get("step_id") in capture_frames]
        complete_frames = len(frame_rows)
        confirmed_tx_count = len(confirmed_steps)
        node_transactions = {node_id: sum(endpoint_id == node_id
                                          for endpoint_id in accepted_by_signature.values())
                             for node_id in nodes}
        all_frame_cus = [item.get("compute_units") for frame_row in frame_rows
                         for item in frame_row.get("transactions", [])]
        known_frame_cus = [value for value in all_frame_cus if isinstance(value, int)]
        all_landing = [value for frame_row in frame_rows
                       for value in frame_row.get("landing_latency_ms", [])
                       if isinstance(value, (int, float))]
        golden_matches = [frame_row.get("golden_match") for frame_row in frame_rows
                          if frame_row.get("golden_match") is not None]
        metrics = {
            "network": config["network"], "genesis_hash": genesis,
            "driver_source_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
            "endpoint_count": args.node_count, "configured_send_rate_per_node": args.send_rate,
            "configured_request_rate_per_node": args.request_rate,
            "run_started_at": run_start.isoformat(), "run_finished_at": run_end.isoformat(),
            "elapsed_seconds": elapsed_seconds,
            "pictures": complete_frames if capture_frames else None,
            "tics": complete_frames * 8 if capture_frames else None,
            "pictures_per_second": complete_frames / elapsed_seconds if capture_frames and elapsed_seconds else None,
            "tics_per_second": (complete_frames * 8) / elapsed_seconds if capture_frames and elapsed_seconds else None,
            "confirmed_transactions": confirmed_tx_count,
            "transactions_per_picture": confirmed_tx_count / complete_frames if complete_frames else None,
            "signed_transaction_count": len(signed_signatures),
            "send_attempt_count": len(send_attempts),
            "same_packet_retry_count": max(0, len(send_attempts) - len(signed_signatures)),
            "same_packet_retry_rate": (max(0, len(send_attempts) - len(signed_signatures)) / len(send_attempts)
                                       if send_attempts else None),
            "rpc_send_error_count": len(error_receipts),
            "rpc_send_error_rate": len(error_receipts) / len(send_attempts) if send_attempts else None,
            "http_429_count": sum(item.get("error_class") == "RateLimited" for item in error_receipts),
            "confirmed_transactions_without_rpc_ack": len(confirmed_signatures - accepted_by_signature.keys()),
            "transaction_rate_per_node": {
                node_id: {"confirmed_rpc_acks": count,
                          "transactions_per_second": count / elapsed_seconds if elapsed_seconds else None}
                for node_id, count in node_transactions.items()
            },
            "compute_units_per_transaction": known_frame_cus,
            "compute_units_per_frame": [item.get("compute_units_per_frame") for item in frame_rows],
            "fee_lamports_per_frame": [item.get("fee_lamports_per_frame") for item in frame_rows],
            "landing_latency_ms": all_landing,
            "landing_latency_ms_median": sorted(all_landing)[len(all_landing) // 2] if all_landing else None,
            "golden_matches": golden_matches,
            "node_metrics_last_process_only": endpoint.counts,
            "pool_state": [vars(item) for item in await pool.snapshots()],
            "journal": str(journal_path),
            "send_receipts": str(args.run_dir / "send-receipts.jsonl"),
        }
        metrics_path = args.run_dir / f"{args.journal}-metrics.json"
        metrics_path.write_text(json.dumps(metrics, sort_keys=True, indent=2) + "\n")
    finally:
        for node in nodes.values():
            await node.aclose()


async def provision(args: argparse.Namespace) -> None:
    config = load_config(args.run_dir)
    resource = Path(config["resource_path"]).read_bytes()
    if len(resource) != RESOURCE_BYTES or resource_tree(resource)[-1][0] != DOOM_RESOURCE_ROOT:
        raise RuntimeError("supplied RI2/WAD bytes do not match the r7c image commitment")
    pool, nodes, endpoint = make_pool(config, node_count=args.node_count, send_rate=args.send_rate,
                                      request_rate=args.request_rate, max_in_flight=args.max_in_flight,
                                      run_dir=args.run_dir)
    try:
        genesis = await endpoint.verify_nodes(config.get("expected_genesis"))
        source = (await endpoint.get_multiple_accounts([str(RESOURCE_KEY)], Commitment.CONFIRMED))[0]
        if source is None:
            raise RuntimeError(f"pinned Doom source account {RESOURCE_KEY} is absent; refusing session setup")
        if source.owner != config["program_id"] or source.executable or source.data != resource:
            raise RuntimeError("pinned Doom source account owner, executable flag, or bytes differ")
        config["observed_genesis"] = genesis
        (args.run_dir / "driver-config.json").write_text(json.dumps(config, sort_keys=True, indent=2) + "\n")
    finally:
        for node in nodes.values():
            await node.aclose()
    plan, _a = setup_plan(config, resource)
    await run_plan(args, plan)


def _read_jsonl(path: Path) -> list[dict[str, Any]]:
    if not path.exists():
        return []
    rows = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.strip():
            rows.append(json.loads(line))
    return rows


def update_campaign_summary(args: argparse.Namespace, config: dict[str, Any],
                            campaign_state: dict[str, Any]) -> dict[str, Any]:
    result_path = args.run_dir / ("local-frame-hashes.jsonl" if config["network"] == "local"
                                  else "frame-results.jsonl")
    results = _read_jsonl(result_path)
    expected_commit_ids = {
        f"frame-{index:04d}-commit-{args.start_cursor + (index + 1) * 8:08d}"
        for index in range(args.frames)
    }
    frame_rows = [row for row in results if row.get("step_id") in expected_commit_ids]
    frame_ids = {row["step_id"].split("-commit-", 1)[0] for row in frame_rows}
    signed_signatures: set[str] = set()
    confirmed_signatures: set[str] = set()
    send_attempt_count = 0
    signed_events_count = 0
    send_error_count = 0
    rate_limited_count = 0
    for index in range(args.frames):
        journal_path = args.run_dir / "journals" / f"{args.journal}-{index:04d}.jsonl"
        if not journal_path.exists():
            continue
        for event in JournalStore(journal_path).events():
            step_id = event.data.get("step_id")
            if not isinstance(step_id, str) or not step_id.startswith(f"frame-{index:04d}-"):
                continue
            if event.name == "step_signed" and event.data.get("signature"):
                signed_signatures.add(str(event.data["signature"]))
                signed_events_count += 1
            elif event.name == "step_confirmed" and event.data.get("signature"):
                confirmed_signatures.add(str(event.data["signature"]))
            elif event.name == "send_attempt_started":
                send_attempt_count += 1
            elif event.name == "send_error":
                send_error_count += 1
                if event.data.get("error_class") == "RateLimited":
                    rate_limited_count += 1
    receipts = [row for row in _read_jsonl(args.run_dir / "send-receipts.jsonl")
                if row.get("signature") in signed_signatures]
    accepted_by_signature: dict[str, str] = {}
    for row in receipts:
        if row.get("kind") == "send_acknowledged":
            accepted_by_signature[str(row["signature"])] = str(row["endpoint_id"])
    now = datetime.now(timezone.utc)
    started = datetime.fromisoformat(campaign_state["started_at"])
    elapsed = max(0.0, (now - started).total_seconds())
    completed_frames = len(frame_ids)
    tx_count = sum(int(row.get("transaction_count", 0)) for row in frame_rows)
    frame_cus = [row.get("compute_units_per_frame") for row in frame_rows]
    tx_cus = [tx.get("compute_units") for row in frame_rows for tx in row.get("transactions", [])
              if isinstance(tx.get("compute_units"), int)]
    fees = [row.get("fee_lamports_per_frame") for row in frame_rows
            if isinstance(row.get("fee_lamports_per_frame"), int)]
    latencies = [value for row in frame_rows for value in row.get("landing_latency_ms", [])
                 if isinstance(value, (int, float))]
    node_accepted = {node_id: sum(node_id == endpoint_id for endpoint_id in accepted_by_signature.values())
                     for node_id in (f"fogo-rpc-{index + 1}" for index in range(args.node_count))}
    summary = {
        "network": config["network"], "genesis_hash": config.get("observed_genesis"),
        "driver_source_sha256": campaign_state.get("driver_source_sha256"),
        "start_cursor": args.start_cursor, "requested_frames": args.frames,
        "completed_frames": completed_frames, "completed_tics": completed_frames * 8,
        "started_at": started.isoformat(), "updated_at": now.isoformat(),
        "elapsed_seconds": elapsed,
        "pictures_per_second": completed_frames / elapsed if elapsed else None,
        "tics_per_second": completed_frames * 8 / elapsed if elapsed else None,
        "confirmed_transactions": tx_count,
        "transactions_per_picture": tx_count / completed_frames if completed_frames else None,
        "transactions_per_second": tx_count / elapsed if elapsed else None,
        "compute_units_per_transaction": tx_cus,
        "compute_units_per_frame": frame_cus,
        "fee_lamports_per_frame": fees,
        "fee_lamports_total": sum(fees),
        "landing_latency_ms": latencies,
        "landing_latency_ms_median": sorted(latencies)[len(latencies) // 2] if latencies else None,
        "signed_signature_count": len(signed_signatures),
        "send_attempt_count": send_attempt_count,
        "same_packet_retry_count": max(0, send_attempt_count - signed_events_count),
        "same_packet_retry_rate": max(0, send_attempt_count - signed_events_count) / send_attempt_count
        if send_attempt_count else None,
        "rpc_send_error_count": send_error_count,
        "rpc_send_error_rate": send_error_count / send_attempt_count if send_attempt_count else None,
        "http_429_count": rate_limited_count,
        "confirmed_transactions_without_rpc_ack": len(confirmed_signatures - accepted_by_signature.keys()),
        "rpc_accepted_signatures_per_node": node_accepted,
        "transaction_rate_per_node": {node_id: count / elapsed if elapsed else None
                                       for node_id, count in node_accepted.items()},
        "frame_hashes": [{"cursor": row.get("cursor"), "sha256": row.get("pixel_sha256"),
                           "golden_match": row.get("golden_match")} for row in frame_rows],
        "results": str(result_path),
    }
    (args.run_dir / f"{args.journal}-summary.json").write_text(
        json.dumps(summary, sort_keys=True, indent=2) + "\n"
    )
    return summary


async def campaign(args: argparse.Namespace) -> None:
    config = load_config(args.run_dir)
    if args.start_cursor < 0 or args.start_cursor % 8:
        raise ValueError("start cursor must be a nonnegative multiple of 8")
    if args.golden is None:
        retained_golden = args.run_dir / ("local-render-golden.json" if config["network"] == "testnet"
                                          else "local-golden.json")
        if retained_golden.exists():
            args.golden = retained_golden
    state_path = args.run_dir / f"{args.journal}-state.json"
    driver_source_sha256 = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    if state_path.exists():
        state = json.loads(state_path.read_text())
        if (state.get("network"), state.get("start_cursor"), state.get("frames")) != (
                config["network"], args.start_cursor, args.frames):
            raise RuntimeError("campaign resume parameters differ from the retained campaign state")
        if state.get("driver_source_sha256") != driver_source_sha256:
            raise RuntimeError("driver source changed since the campaign journal was created")
    else:
        state = {"network": config["network"], "start_cursor": args.start_cursor,
                 "frames": args.frames, "started_at": datetime.now(timezone.utc).isoformat(),
                 "driver_source_sha256": driver_source_sha256}
        state_path.write_text(json.dumps(state, sort_keys=True, indent=2) + "\n")
    update_campaign_summary(args, config, state)
    result_path = args.run_dir / ("local-frame-hashes.jsonl" if config["network"] == "local"
                                  else "frame-results.jsonl")
    complete_ids = {row.get("step_id") for row in _read_jsonl(result_path)}
    base_journal = args.journal
    for frame_index in range(args.frames):
        cursor = args.start_cursor + frame_index * 8
        target_cursor = cursor + 8
        commit_id = f"frame-{frame_index:04d}-commit-{target_cursor:08d}"
        if commit_id in complete_ids:
            continue
        plan, addresses, commits = campaign_plan(config, 1, cursor, frame_offset=frame_index)
        args.journal = f"{base_journal}-{frame_index:04d}"
        try:
            await run_plan(args, plan, capture_frames=commits, addresses=addresses)
        finally:
            args.journal = base_journal
        complete_ids.add(commit_id)
        update_campaign_summary(args, config, state)
    update_campaign_summary(args, config, state)


async def cleanup(args: argparse.Namespace) -> None:
    config = load_config(args.run_dir)
    program = Pubkey.from_string(config["program_id"])
    payer = Pubkey.from_string(config["pubkeys"]["payer"])
    authority = Pubkey.from_string(config["pubkeys"]["authority"])
    a = Addresses.derive(program, authority, config["session_id"])
    cursor = args.cursor
    steps: list[TransactionStep] = []
    _append_step(steps, program=program, payer=payer,
                 instructions=[halt_session(program, authority, a, cursor)],
                 name=f"halt-session-{cursor:08d}", locks=(a.session,), compute_class="doom-halt",
                 postcondition=None)
    children = [(a.workspace, 5), (a.scratch, 4), *((key, 3) for key in reversed(a.views)),
                (a.resource, 6), *((key, 2) for key in reversed(a.states)), (a.stream, 1)]
    for index, (target, kind) in enumerate(children):
        _append_step(steps, program=program, payer=payer,
                     instructions=[close_child(program, a, target, authority, kind)],
                     name=f"close-child-{index:02d}-{kind}", locks=(a.session, target, authority),
                     compute_class="doom-close-child")
    _append_step(steps, program=program, payer=payer,
                 instructions=[close_session(program, a, authority)],
                 name="close-session", locks=(a.session, authority), compute_class="doom-close-session")
    plan = TransactionPlan(
        genesis_hash=config.get("observed_genesis") or config.get("expected_genesis") or "local-genesis-pending",
        program_id=str(program), destination_accounts=(str(a.session), *(str(item[0]) for item in children)),
        signer_public_key=str(payer), steps=tuple(steps),
    )
    payer_before = await _balance(args, config, authority)
    await run_plan(args, plan)
    payer_after = await _balance(args, config, authority)
    receipt = {"authority_balance_before_lamports": payer_before,
               "authority_balance_after_lamports": payer_after,
               "rent_returned_lamports": payer_after - payer_before,
               "cursor": cursor}
    (args.run_dir / "cleanup-rent.json").write_text(json.dumps(receipt, sort_keys=True, indent=2) + "\n")
    print(json.dumps(receipt, sort_keys=True))


async def _balance(args: argparse.Namespace, config: dict[str, Any], address: Pubkey) -> int:
    pool, nodes, endpoint = make_pool(config, node_count=args.node_count, send_rate=args.send_rate,
                                      request_rate=args.request_rate, max_in_flight=args.max_in_flight,
                                      run_dir=args.run_dir)
    try:
        info = (await endpoint.get_multiple_accounts([str(address)], Commitment.CONFIRMED))[0]
        return info.lamports if info else 0
    finally:
        for node in nodes.values():
            await node.aclose()


def run_async(coro) -> None:
    asyncio.run(coro)


def parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-dir", type=Path, default=DEFAULT_RUN)
    sub = parser.add_subparsers(dest="command", required=True)
    p_prepare = sub.add_parser("prepare", help="create fresh local or testnet run identities")
    p_prepare.add_argument("--network", choices=("local", "testnet"), required=True)
    p_prepare.add_argument("--rpc-url", default="http://127.0.0.1:18899",
                           help="local validator RPC URL (default: %(default)s)")
    p_prepare.set_defaults(func=prepare)
    for name, func in (("preflight", preflight), ("deploy-one", deploy_one),
                       ("provision", provision), ("campaign", campaign), ("cleanup", cleanup)):
        p = sub.add_parser(name)
        p.add_argument("--node-count", type=int, default=1)
        p.add_argument("--send-rate", type=float, default=20.0)
        p.add_argument("--request-rate", type=float, default=60.0)
        p.add_argument("--max-in-flight", type=int, default=8)
        p.set_defaults(func=func)
    sub.choices["deploy-one"].add_argument("--image-kind", choices=("doom", "dcg"), required=True)
    sub.choices["campaign"].add_argument("--frames", type=int, required=True)
    sub.choices["campaign"].add_argument("--start-cursor", type=int, default=0)
    sub.choices["campaign"].add_argument("--golden", type=Path)
    sub.choices["campaign"].add_argument("--journal", default="campaign")
    sub.choices["campaign"].set_defaults(journal="campaign")
    sub.choices["provision"].add_argument("--journal", default="provision")
    sub.choices["provision"].set_defaults(journal="provision")
    sub.choices["cleanup"].add_argument("--cursor", type=int, required=True)
    sub.choices["cleanup"].add_argument("--journal", default="cleanup")
    sub.choices["cleanup"].set_defaults(journal="cleanup")
    sub.choices["preflight"].set_defaults(journal="preflight")
    sub.choices["deploy-one"].set_defaults(journal="deploy")
    return parser


def main() -> None:
    args = parser().parse_args()
    if args.command in {"preflight", "deploy-one", "provision", "campaign", "cleanup"}:
        run_async(args.func(args))
    else:
        args.func(args)


if __name__ == "__main__":
    main()
