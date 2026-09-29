// Single-game public lobby. The chain session remains the existing one-slot
// pipeline; this class only arbitrates browsers and pauses that pipeline.

import {randomBytes, randomUUID} from "node:crypto";
import {dirname} from "node:path";
import {existsSync, mkdirSync, readFileSync, renameSync, writeFileSync} from "node:fs";

const ADJECTIVES = ["Amber", "Brisk", "Cosmic", "Daring", "Electric", "Fuzzy", "Golden", "Lucky", "Neon", "Quiet", "Rapid", "Silver", "Turbo", "Velvet"];
const NOUNS = ["Baron", "Cacodemon", "Comet", "Imp", "Marine", "Mancubus", "Pixel", "Ranger", "Revenant", "Rocket", "Skull", "Spectre", "Trooper", "Wad"];

export class PublicModeError extends Error {
  constructor(message, status = 400, code = "public_mode_error") {
    super(message);
    this.status = status;
    this.code = code;
  }
}

export class PublicMode {
  constructor({
    relayer,
    statePath = null,
    maxViewers = 200,
    idleMs = 60_000,
    offerMs = 5_000,
    heartbeatMs = 15_000,
    inputLimitPerSecond = 12,
    spendCapFogo = 300,
    now = () => Date.now(),
    random = Math.random,
    onRolloverFailure = null,
  }) {
    this.relayer = relayer;
    this.statePath = statePath;
    this.maxViewers = maxViewers;
    this.idleMs = idleMs;
    this.offerMs = offerMs;
    this.heartbeatMs = heartbeatMs;
    this.inputLimitPerSecond = inputLimitPerSecond;
    this.spendCapFogo = spendCapFogo;
    this.now = now;
    this.random = random;
    this.onRolloverFailure = onRolloverFailure;
    this.clients = new Map();
    this.activeToken = null;
    this.offerToken = null;
    this.offerUntil = null;
    this.offeredThisRound = new Set();
    this.lastInputAt = null;
    this.lastFrame = null;
    this.deathUntil = null;
    this.paused = true;
    this.capReached = false;
    this.pipelineStopped = false;
    this.rollingOver = false;
    this.rolloverFailureHandled = false;
    this.listeners = new Set();
    this.lastStatusJson = "";
    this.run = this.#loadState();
    if (this.run.spend_cap_fogo == null) {
      this.run.spend_cap_fogo = this.spendCapFogo;
      this.#persist();
    } else if (Number(this.run.spend_cap_fogo) !== this.spendCapFogo) {
      throw new Error(`public run cap is fixed at ${this.run.spend_cap_fogo} FOGO; use its original cap or choose a new public-state file`);
    }
    this.capReached = Number(this.run.spend_fogo ?? 0) >= this.spendCapFogo;
    if (this.run.last_frame) {
      this.lastFrame = this.run.last_frame;
      this.relayer.frameStream.set(this.lastFrame.frame_index, this.lastFrame);
    }
  }

  #loadState() {
    if (!this.statePath) return {run_id: randomUUID(), schema: "basanos/doom-public-run/1"};
    if (!existsSync(this.statePath)) return {run_id: randomUUID(), schema: "basanos/doom-public-run/1"};
    try {
      const value = JSON.parse(readFileSync(this.statePath, "utf8"));
      if (value.schema !== "basanos/doom-public-run/1" || typeof value.run_id !== "string") {
        throw new Error("unexpected schema or missing run_id");
      }
      if (value.spend_fogo > 0 && value.payer_baseline_lamports == null) {
        throw new Error("spent run is missing its original payer-balance baseline");
      }
      if (value.payer_baseline_lamports != null && !Number.isSafeInteger(value.payer_baseline_lamports)) {
        throw new Error("payer-balance baseline is not a safe integer");
      }
      return value;
    } catch (error) {
      throw new Error(`cannot read public run state ${this.statePath}: ${error.message}`);
    }
  }

  #persist() {
    if (!this.statePath) return;
    mkdirSync(dirname(this.statePath), {recursive: true, mode: 0o700});
    const temporary = `${this.statePath}.${process.pid}.tmp`;
    writeFileSync(temporary, `${JSON.stringify(this.run)}\n`, {mode: 0o600});
    renameSync(temporary, this.statePath);
  }

  #token() {
    return randomBytes(32).toString("base64url");
  }

  #nickname() {
    const pick = (items) => items[Math.min(items.length - 1, Math.floor(this.random() * items.length))];
    return `${pick(ADJECTIVES)} ${pick(NOUNS)}`;
  }

  #client(token) {
    const client = this.clients.get(token);
    if (!client) throw new PublicModeError("browser session expired; join again", 401, "unknown_public_session");
    return client;
  }

  #active() {
    return this.activeToken ? this.clients.get(this.activeToken) ?? null : null;
  }

  async join({publicKey}) {
    if (typeof publicKey !== "string" || !/^[0-9a-f]{64}$/i.test(publicKey)) {
      throw new PublicModeError("publicKey must be 32 bytes of hex", 400, "bad_public_key");
    }
    const existing = [...this.clients.values()].find((client) => client.publicKey === publicKey);
    if (!existing && this.clients.size >= this.maxViewers) {
      throw new PublicModeError(`public mode is full (${this.maxViewers} viewers)`, 429, "viewer_cap");
    }

    let client;
    if (existing) {
      const oldToken = existing.token;
      const token = this.#token();
      this.clients.delete(oldToken);
      client = {...existing, token, heartbeatAt: this.now()};
      this.clients.set(token, client);
      if (this.activeToken === oldToken) {
        this.activeToken = token;
        if (this.relayer.publicSession) this.relayer.publicSession.clientSeq = 0;
        this.lastInputAt = this.now();
      }
      if (this.offerToken === oldToken) this.offerToken = token;
      if (this.offeredThisRound.delete(oldToken)) this.offeredThisRound.add(token);
    } else {
      const token = this.#token();
      client = {token, publicKey: publicKey.toLowerCase(), nickname: this.#nickname(), joinedAt: this.now(), heartbeatAt: this.now(), inputTimes: []};
      this.clients.set(token, client);
      if (!this.relayer.publicSession) {
        const created = await this.relayer.createSession({publicKey: client.publicKey});
        this.relayer.publicSession = this.relayer.sessions.get(created.id);
        this.relayer.publicSession.public = true;
      }
      if (!this.activeToken) {
        await this.#assignActive(client);
        this.relayer.publicSession.playerKey = client.publicKey;
        this.relayer.publicSession.clientSeq = 0;
      }
    }
    this.relayer.publicSession ??= [...this.relayer.sessions.values()][0] ?? null;
    if (!this.relayer.publicSession) {
      this.clients.delete(client.token);
      throw new PublicModeError("the shared game session is not ready", 503, "game_not_ready");
    }
    this.relayer.publicSession.lastSeenAt = this.now();
    this.#changed();
    return {token: client.token, public_key: client.publicKey, nickname: client.nickname, session: this.#sessionInfo(), state: this.state(client.token)};
  }

  #sessionInfo() {
    const session = this.relayer.publicSession;
    if (!session) return null;
    return {
      id: session.id,
      slot: session.slot.slot,
      binding: Buffer.from(this.relayer.bindings?.get(session.slot.label) ?? []).toString("hex"),
      next_seq: session.slot.nextSeq,
    };
  }

  heartbeat(token) {
    const client = this.#client(token);
    client.heartbeatAt = this.now();
    if (this.relayer.publicSession) this.relayer.publicSession.lastSeenAt = this.now();
    return this.state(token);
  }

  async leave(token) {
    const client = this.#client(token);
    this.clients.delete(token);
    this.offeredThisRound.delete(token);
    if (this.offerToken === token) {
      this.offerToken = null;
      this.offerUntil = null;
    }
    if (this.activeToken === token) {
      this.activeToken = null;
      this.lastInputAt = null;
      const next = this.#oldestClient();
      if (next) await this.#assignActive(next);
    }
    if (!this.clients.size) await this.#pauseSender();
    this.#changed();
    return {ok: true, nickname: client.nickname};
  }

  #oldestClient(exclude = null) {
    return [...this.clients.values()]
      .filter((client) => client.token !== exclude)
      .sort((a, b) => a.joinedAt - b.joinedAt)[0] ?? null;
  }

  async #assignActive(client) {
    await this.#pauseSender();
    this.activeToken = client.token;
    this.offerToken = null;
    this.offerUntil = null;
    this.offeredThisRound.clear();
    this.lastInputAt = this.now();
    if (this.relayer.publicSession) {
      this.relayer.publicSession.playerKey = client.publicKey;
      this.relayer.publicSession.clientSeq = 0;
      this.relayer.publicSession.lastSeenAt = this.now();
    }
    this.paused = true;
  }

  assertCanInput(token, publicKey, {rateLimit = true} = {}) {
    const client = this.#client(token);
    if (this.capReached) throw new PublicModeError("the public run spend cap has been reached", 402, "spend_cap_reached");
    if (this.pipelineStopped) throw new PublicModeError("the shared game session has stopped", 503, "pipeline_stopped");
    if (this.rollingOver) throw new PublicModeError("the shared game is starting a new game; retry in a moment", 503, "rollover_in_progress");
    if (client.token !== this.activeToken || this.offerToken) {
      throw new PublicModeError("only the current player may send input", 403, "not_current_player");
    }
    if (publicKey !== client.publicKey) {
      throw new PublicModeError("input key does not match this browser session", 401, "wrong_public_key");
    }
    if (rateLimit) {
      const now = this.now();
      client.inputTimes = client.inputTimes.filter((time) => now - time < 1000);
      if (client.inputTimes.length >= this.inputLimitPerSecond) {
        throw new PublicModeError("input rate limit exceeded", 429, "input_rate_limited");
      }
      client.inputTimes.push(now);
    }
    return client;
  }

  async resumeForInput(token) {
    this.assertCanInput(token, this.#client(token).publicKey, {rateLimit: false});
    if (this.paused) {
      await this.relayer.sender.resume?.();
      this.paused = false;
      this.#changed();
    }
  }

  notePendingInput(token) {
    this.assertCanInput(token, this.#client(token).publicKey, {rateLimit: false});
    this.lastInputAt = this.now();
    this.#changed();
  }

  noteAcceptedInput(token) {
    this.assertCanInput(token, this.#client(token).publicKey, {rateLimit: false});
    this.lastInputAt = this.now();
    if (this.relayer.publicSession) this.relayer.publicSession.lastSeenAt = this.now();
    this.deathUntil = null;
    this.#changed();
  }

  async acceptOffer(token) {
    const client = this.#client(token);
    if (this.offerToken !== token || this.offerUntil === null || this.offerUntil <= this.now()) {
      throw new PublicModeError("there is no active control offer for this browser", 409, "offer_expired");
    }
    await this.#assignActive(client);
    this.#changed();
    return this.state(token);
  }

  async #pauseSender() {
    if (this.paused) return;
    await this.relayer.sender.pause?.();
    this.paused = true;
  }

  async #offerNext() {
    const candidates = [...this.clients.values()].filter((client) => client.token !== this.activeToken && !this.offeredThisRound.has(client.token));
    if (!candidates.length) {
      const spectators = [...this.clients.values()].filter((client) => client.token !== this.activeToken);
      if (!spectators.length) {
        this.offerToken = null;
        this.offerUntil = null;
        this.offeredThisRound.clear();
        this.lastInputAt = this.now();
        return;
      }
      this.offeredThisRound.clear();
      return this.#offerNext();
    }
    const selected = candidates[Math.min(candidates.length - 1, Math.floor(this.random() * candidates.length))];
    this.offerToken = selected.token;
    this.offerUntil = this.now() + this.offerMs;
  }

  async tick() {
    const now = this.now();
    let changed = false;
    for (const [token, client] of this.clients) {
      if (now - client.heartbeatAt > this.heartbeatMs * 2) {
        this.clients.delete(token);
        this.offeredThisRound.delete(token);
        if (this.offerToken === token) {
          this.offerToken = null;
          this.offerUntil = null;
        }
        if (this.activeToken === token) {
          this.activeToken = null;
          this.lastInputAt = null;
        }
        changed = true;
      }
    }
    if (!this.activeToken && this.clients.size) {
      const next = this.#oldestClient();
      if (next) {
        await this.#assignActive(next);
        changed = true;
      }
    }
    if (this.offerToken && (!this.clients.has(this.offerToken) || now >= this.offerUntil)) {
      this.offeredThisRound.add(this.offerToken);
      this.offerToken = null;
      this.offerUntil = null;
      await this.#offerNext();
      changed = true;
    }
    const active = this.#active();
    if (!active) {
      await this.#pauseSender();
    } else if (this.lastInputAt !== null && now - this.lastInputAt >= this.idleMs) {
      await this.#pauseSender();
      const spectators = [...this.clients.values()].some((client) => client.token !== this.activeToken);
      if (spectators && !this.offerToken) {
        await this.#offerNext();
        changed = true;
      }
    }
    if (changed) this.#changed();
    else this.#broadcastIfChanged();
  }

  onFrame(record) {
    this.lastFrame = record;
    this.run.last_frame = record;
    this.#persist();
    this.#changed();
  }

  onPipelineEvent(event) {
    if (event.event === "rollover_start") {
      this.rollingOver = true;
      this.paused = true;
      this.#changed();
    }
    if (event.event === "rollover_complete") {
      this.rollingOver = false;
      this.paused = true;
      this.lastInputAt = this.now();
      const slot = this.relayer.publicSession?.slot;
      if (slot) {
        slot.cursor = 0;
        slot.gametic = 0;
        slot.nextSeq = 0;
        slot.stagingFrontier = 0;
        slot.writableFrontier = 0;
        slot.writableFloor = 0;
        slot.producerNextCursor = 0;
        slot.producer = null;
        slot.inputFate = null;
        slot.pipeline = null;
      }
      this.#changed();
    }
    if (event.event === "rollover_failed") {
      this.rollingOver = false;
      this.pipelineStopped = true;
      this.paused = true;
      this.#changed();
      if (!this.rolloverFailureHandled) {
        this.rolloverFailureHandled = true;
        void this.onRolloverFailure?.(event);
      }
    }
    if (event.event === "death") {
      this.deathUntil = this.now() + 15_000;
      this.#changed();
    }
    if (event.event === "failed" || event.event === "finished") {
      this.pipelineStopped = true;
      if (Number(event.spend_fogo) >= this.spendCapFogo) this.capReached = true;
      this.paused = true;
      this.#changed();
    }
    if (event.event === "started") {
      const baseline = event.payer_fogo ?? event.preflight?.payer_fogo;
      if (Number.isFinite(Number(baseline)) && this.run.payer_baseline_lamports == null) {
        this.run.payer_baseline_lamports = Math.round(Number(baseline) * 1_000_000_000);
        this.#persist();
      }
    }
    if (event.event === "progress") {
      const spend = Number(event.spend_fogo);
      if (Number.isFinite(spend)) {
        this.run.spend_fogo = spend;
        this.run.last_spend_at = new Date(this.now()).toISOString();
        this.run.last_cursor = event.cursor ?? this.run.last_cursor ?? null;
        this.run.last_frame_index = this.lastFrame?.frame_index ?? this.run.last_frame_index ?? null;
        const hourKey = new Date(this.now()).toISOString().slice(0, 13);
        const hours = this.run.hourly_spend ?? (this.run.hourly_spend = {});
        const previous = hours[hourKey] ?? {start_fogo: spend, end_fogo: spend, spent_fogo: 0, samples: 0};
        previous.end_fogo = spend;
        previous.spent_fogo = Math.max(0, Math.round((previous.end_fogo - previous.start_fogo) * 1_000_000_000) / 1_000_000_000);
        previous.samples += 1;
        hours[hourKey] = previous;
        if (spend >= this.spendCapFogo) this.capReached = true;
        this.#persist();
        this.#changed();
      }
    }
  }

  state(token = null) {
    const now = this.now();
    const clients = [...this.clients.values()].sort((a, b) => a.joinedAt - b.joinedAt);
    const client = token ? this.clients.get(token) : null;
    const active = this.#active();
    const spectatorList = clients.filter((entry) => entry.token !== this.activeToken);
    const ownPlace = client?.token === this.activeToken ? null : spectatorList.findIndex((entry) => entry.token === token) + 1;
    const offered = this.offerToken ? this.clients.get(this.offerToken) : null;
    const idleRemaining = this.lastInputAt === null ? this.idleMs : Math.max(0, this.idleMs - (now - this.lastInputAt));
    const pausedReason = this.capReached ? "cap" : this.pipelineStopped ? "stopped" : this.rollingOver ? "rollover" : !active ? "empty" : this.paused ? "idle" : null;
    return {
      viewers: clients.length,
      max_viewers: this.maxViewers,
      active_player: active?.nickname ?? null,
      is_player: Boolean(client && client.token === this.activeToken && !this.offerToken && !this.capReached && !this.pipelineStopped),
      idle_seconds: Math.ceil(idleRemaining / 1000),
      offer: offered ? {nickname: offered.nickname, seconds: Math.max(0, Math.ceil((this.offerUntil - now) / 1000)), mine: offered.token === token} : null,
      spectator_place: !client || client.token === this.activeToken ? null : ownPlace,
      phase: this.capReached ? "cap_reached" : this.pipelineStopped ? "stopped" : this.rollingOver ? "rolling_over" : offered ? (offered.token === token ? "offer" : "offer_rotation") : active ? (this.paused ? "paused" : "playing") : "empty",
      paused: this.paused || this.capReached || this.rollingOver,
      paused_reason: pausedReason,
      rolling_over: this.rollingOver,
      pause_message: this.capReached ? "Public run spend cap reached. The game is paused." : this.pipelineStopped ? "The shared game session has stopped. Please try again later." : this.rollingOver ? "New game starting…" : this.paused ? "Paused — press any key / take control to play" : null,
      death_message: this.deathUntil && this.deathUntil > now ? "You died. Press R to respawn." : null,
      spend_fogo: Number(this.run.spend_fogo ?? 0),
      spend_cap_fogo: this.spendCapFogo,
      run_id: this.run.run_id,
      frame_index: this.lastFrame?.frame_index ?? null,
      cursor: this.lastFrame?.cursor ?? this.run.last_cursor ?? null,
      nickname: client?.nickname ?? null,
    };
  }

  #changed() {
    this.lastStatusJson = "";
    this.#broadcastIfChanged();
  }

  #broadcastIfChanged() {
    const json = JSON.stringify(this.state());
    if (json === this.lastStatusJson) return;
    this.lastStatusJson = json;
    for (const listener of this.listeners) listener();
  }

  subscribe(token, listener) {
    this.#client(token);
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  start() {
    if (this.timer) return;
    this.timer = setInterval(() => void this.tick().catch((error) => this.relayer.log(`public mode tick failed: ${error.message}`)), 250);
    this.timer.unref?.();
  }

  stop() {
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
  }

  async palette() {
    if (this.run.palette) return this.run.palette;
    if (this.relayer.mode === "mock") return Array.from({length: 768}, (_, index) => Math.floor(index / 3));
    if (!this.palettePromise) {
      this.palettePromise = (async () => {
        const [{ChainReader, WadImageReader, readPlaypal}, {NETWORKS}] = await Promise.all([
          import("../../web/doom-play/rpc.js"),
          import("../../web/doom-play/network.js"),
        ]);
        const network = NETWORKS[this.relayer.networkName];
        const reader = new ChainReader({network: this.relayer.networkName});
        const wad = new WadImageReader({
          // WadImageReader already offsets past each DOOM-WC1 account header
          // when it asks for a slice. Return the RPC dataSlice unchanged, as
          // the existing Doom page does, or the second header skip truncates
          // the first slice to zero bytes.
          readSlice: (address, offset, length) => reader.getAccount(address, {offset, length}),
          accounts: network.doom.wadAccounts,
          accountBytes: network.doom.wadAccountBytes,
          headerBytes: network.doom.wadHeaderBytes,
          imageBytes: network.doom.wadImageBytes,
        });
        return [...await readPlaypal(wad)];
      })();
    }
    this.run.palette = await this.palettePromise;
    this.#persist();
    return this.run.palette;
  }
}
